//! Talking to a Compx mouse over a [`RawDuplex`].
//!
//! Every operation is one session: open the vendor collection, announce the
//! host driver, exchange requests, withdraw the driver. Sessions are
//! serialized process-wide, because two of them interleaving on one receiver
//! would steal each other's replies — replies carry no request id, only the
//! echoed opcode.

use std::time::Duration;

use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, StandaloneDevice};
use openlogi_core::hid::dpi::{Dpi as WireDpi, DpiCapabilities, DpiInfo};
use openlogi_core::hid::report_rate::{ReportRate as WireReportRate, ReportRateInfo};
use openlogi_core::hid::{DeviceRoute, WriteError};
use openlogi_device_registry::compx::find_compx;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::config::{self, ConfigChangeError, Dpi, FlashImage, MouseConfig, ReportRate};
use super::{Battery, PcDriver, REPORT_LEN, Request, Response, ResponseError};
use crate::backend::{HidBackend, NodeInfo, RawDuplex};
use crate::channel::route::find_raw_route_node;

/// How long each attempt waits for its reply, one entry per attempt. The
/// vendor driver waits 100 ms, but on hardware an idle mouse missed three
/// 250 ms attempts and answered a request sent seconds later, so the wait
/// grows instead of giving up within a second. Resending is safe: every
/// request, flash writes included, is idempotent.
const REPLY_TIMEOUTS: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
];
/// Rates offered on a receiver link. Higher rates need a 4K receiver this
/// driver cannot yet tell apart, so they are offered only when the mouse
/// already runs at one.
const RECEIVER_RATES: [ReportRate; 4] = [
    ReportRate::Hz125,
    ReportRate::Hz250,
    ReportRate::Hz500,
    ReportRate::Hz1000,
];

static SESSION: Mutex<()> = Mutex::const_new(());

/// One open, attached session with a Compx receiver.
struct Link {
    duplex: Box<dyn RawDuplex>,
}

impl Link {
    async fn open(backend: &dyn HidBackend, node: &NodeInfo) -> Result<Self, WriteError> {
        let mut link = Self {
            duplex: backend.open_raw_duplex(node).await?,
        };
        link.transact(&Request::pc_driver(PcDriver::Attached))
            .await?;
        Ok(link)
    }

    async fn close(mut self) {
        // Best effort: a mouse that stopped answering mid-session still has
        // to be released, and nothing useful follows a failed release.
        if let Err(error) = self.transact(&Request::pc_driver(PcDriver::Detached)).await {
            debug!(%error, "Compx driver release went unanswered");
        }
    }

    async fn transact(&mut self, request: &Request) -> Result<Response, WriteError> {
        for timeout in REPLY_TIMEOUTS {
            self.duplex.write_output_report(request.as_bytes()).await?;
            match tokio::time::timeout(timeout, self.reply_to(request)).await {
                Ok(reply) => return reply,
                Err(_elapsed) => {}
            }
        }
        warn!(opcode = ?request.opcode(), "Compx mouse did not answer");
        Err(WriteError::RawDeviceUnresponsive)
    }

    /// Read reports until one answers `request`. Reports for other requests
    /// and other report ids on the collection are skipped; a rejection of
    /// this request is its answer.
    async fn reply_to(&mut self, request: &Request) -> Result<Response, WriteError> {
        let mut buffer = [0u8; 64];
        loop {
            let length = self.duplex.read_input_report(&mut buffer).await?;
            let Some(report) = buffer.get(..length).filter(|r| r.len() >= REPORT_LEN) else {
                continue;
            };
            match Response::parse(report) {
                Ok(response) if request.is_answered_by(&response) => return Ok(response),
                Err(ResponseError::Rejected { opcode, status }) if opcode == request.opcode() => {
                    return Err(protocol(ResponseError::Rejected { opcode, status }));
                }
                _ => {}
            }
        }
    }

    async fn read_config(&mut self) -> Result<MouseConfig, WriteError> {
        let mut image = FlashImage::default();
        for request in FlashImage::read_requests() {
            let response = self.transact(&request).await?;
            image.accept(&response).map_err(protocol)?;
        }
        MouseConfig::parse(&image).map_err(protocol)
    }

    async fn battery(&mut self) -> Result<Battery, WriteError> {
        let response = self.transact(&Request::battery()).await?;
        Battery::decode(&response).map_err(protocol)
    }
}

fn protocol(error: impl std::fmt::Display) -> WriteError {
    WriteError::RawDeviceProtocol(error.to_string())
}

impl From<ConfigChangeError> for WriteError {
    fn from(error: ConfigChangeError) -> Self {
        protocol(error)
    }
}

/// Run `body` in one serialized, attached session with the Compx mouse at
/// `route`.
async fn with_link<T>(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    body: impl AsyncFnOnce(&mut Link) -> Result<T, WriteError>,
) -> Result<T, WriteError> {
    let _session = SESSION.lock().await;
    let node = find_raw_route_node(backend, route)
        .await?
        .filter(is_compx_node)
        .ok_or(WriteError::DeviceNotFound)?;
    let mut link = Link::open(backend, &node).await?;
    let result = body(&mut link).await;
    link.close().await;
    result
}

fn is_compx_node(node: &NodeInfo) -> bool {
    find_compx(
        node.vendor_id,
        node.product_id,
        node.usage_page,
        node.usage_id,
    )
    .is_some()
}

/// Whether `route` addresses a Compx mouse, so callers can dispatch to this
/// driver instead of HID++.
#[must_use]
pub fn is_compx_route(route: &DeviceRoute) -> bool {
    matches!(
        route,
        DeviceRoute::RawHid { vendor_id, product_id, usage_page, usage_id, .. }
            if find_compx(*vendor_id, *product_id, *usage_page, *usage_id).is_some()
    )
}

/// The active stage's DPI and every value the flash can store.
///
/// # Errors
///
/// [`WriteError`] when the mouse is unreachable or its flash is unreadable.
pub async fn read_dpi_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DpiInfo, WriteError> {
    let config = with_link(backend, route, async |link| link.read_config().await).await?;
    dpi_info(&config)
}

/// Set the active stage's resolution to `dpi` on both axes, keeping its
/// colour, then read it back.
///
/// # Errors
///
/// [`WriteError::DpiUnsupported`] for a value the flash cannot store, and
/// [`WriteError::DpiNotApplied`] when the read-back disagrees.
pub async fn set_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    dpi: WireDpi,
) -> Result<(), WriteError> {
    let requested =
        Dpi::new(dpi.into_inner()).ok_or(WriteError::DpiUnsupported { requested: dpi })?;
    with_link(backend, route, async |link| {
        let config = link.read_config().await?;
        let stage = config.active_stage;
        link.transact(&config::set_stage_resolution(
            &config, stage, requested, requested,
        )?)
        .await?;
        let actual = link.read_config().await?.current_stage().x;
        if actual == requested {
            Ok(())
        } else {
            Err(WriteError::DpiNotApplied {
                requested: dpi,
                actual: WireDpi::new(actual.get()),
            })
        }
    })
    .await
}

/// The current polling rate and the rates offered on this link.
///
/// # Errors
///
/// [`WriteError`] when the mouse is unreachable or its flash is unreadable.
pub async fn read_report_rate_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    let config = with_link(backend, route, async |link| link.read_config().await).await?;
    Ok(report_rate_info(config.report_rate))
}

/// Set the polling rate, then read it back and return what the mouse
/// reports.
///
/// # Errors
///
/// [`WriteError::ReportRateUnsupported`] for a rate not offered on this
/// link, and [`WriteError::ReportRateNotApplied`] when the read-back
/// disagrees.
pub async fn set_report_rate(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    rate: WireReportRate,
) -> Result<ReportRateInfo, WriteError> {
    with_link(backend, route, async |link| {
        let current = link.read_config().await?.report_rate;
        let requested = RECEIVER_RATES
            .into_iter()
            .chain([current])
            .find(|offered| hertz(*offered) == rate)
            .ok_or(WriteError::ReportRateUnsupported { requested: rate })?;
        link.transact(&config::set_report_rate(requested)).await?;
        let actual = link.read_config().await?.report_rate;
        if actual == requested {
            Ok(report_rate_info(actual))
        } else {
            Err(WriteError::ReportRateNotApplied {
                requested: rate,
                actual: hertz(actual),
            })
        }
    })
    .await
}

/// Fill the live fields of every Compx device in `devices`: battery, and
/// the DPI and polling-rate capabilities once its configuration reads.
///
/// A mouse that does not answer — asleep, switched off, or out of range —
/// keeps no battery and no capabilities, exactly as enumeration left it.
pub async fn refresh_standalone(backend: &dyn HidBackend, devices: &mut [StandaloneDevice]) {
    for device in devices
        .iter_mut()
        .filter(|device| is_compx_route(&device.route()))
    {
        let route = device.route();
        let status = with_link(backend, &route, async |link| {
            let battery = link.battery().await?;
            let config = link.read_config().await?;
            Ok((battery, config))
        })
        .await;
        match status {
            Ok((battery, _config)) => {
                device.battery = Some(battery_info(battery));
                let capabilities = device.capabilities.get_or_insert_default();
                capabilities.pointer = true;
                capabilities.report_rate = true;
            }
            Err(error) => debug!(%error, "Compx mouse did not report its state"),
        }
    }
}

fn dpi_info(config: &MouseConfig) -> Result<DpiInfo, WriteError> {
    Ok(DpiInfo {
        current: WireDpi::new(config.current_stage().x.get()),
        capabilities: DpiCapabilities::new(config::storable_dpi().map(Dpi::get).collect())?,
    })
}

fn report_rate_info(current: ReportRate) -> ReportRateInfo {
    let mut supported: Vec<WireReportRate> = RECEIVER_RATES
        .into_iter()
        .chain([current])
        .map(hertz)
        .collect();
    supported.sort_unstable();
    supported.dedup();
    ReportRateInfo {
        current: hertz(current),
        supported,
    }
}

fn hertz(rate: ReportRate) -> WireReportRate {
    WireReportRate::new(rate.hertz())
}

fn battery_info(battery: Battery) -> BatteryInfo {
    BatteryInfo {
        percentage: Some(battery.percent),
        level: match battery.percent {
            0..=5 => BatteryLevel::Critical,
            6..=20 => BatteryLevel::Low,
            21..=94 => BatteryLevel::Good,
            _ => BatteryLevel::Full,
        },
        status: if battery.charging {
            BatteryStatus::Charging
        } else {
            BatteryStatus::Discharging
        },
    }
}

#[cfg(test)]
mod tests;
