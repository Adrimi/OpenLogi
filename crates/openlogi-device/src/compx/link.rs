//! Talking to a Compx mouse over a [`RawDuplex`].
//!
//! Every operation is one session: open the vendor collection, announce the
//! host driver, exchange requests, withdraw the driver. Sessions are
//! serialized process-wide, because two of them interleaving on one receiver
//! would steal each other's replies — replies carry no request id, only the
//! echoed opcode.

use std::time::Duration;

use openlogi_core::color::Rgb as WireRgb;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, StandaloneDevice};
use openlogi_core::hid::dpi::{Dpi as WireDpi, DpiCapabilities, DpiInfo};
use openlogi_core::hid::report_rate::{ReportRate as WireReportRate, ReportRateInfo};
use openlogi_core::hid::{
    DeviceRoute, MouseSettings, MouseSettingsChange, OnboardDpiStage, OnboardDpiStages,
    OnboardDpiStagesChange, WriteError,
};
use openlogi_device_registry::compx::find_compx;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, warn};

use super::config::{
    self, ConfigChangeError, Dpi, DpiStage, FlashImage, MouseConfig, ReportRate, Rgb,
};
use super::{Battery, PcDriver, REPORT_LEN, Request, Response, ResponseError, SerialNumber};
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

/// The least time between two sessions that check one known mouse. The
/// agent's recovery scan runs about twice a minute, and a battery moves far
/// slower than that.
const CHECK_INTERVAL: Duration = Duration::from_mins(2);
/// How long a mouse that stopped answering keeps its last-good battery and
/// controls before it is shown without them.
const LAST_GOOD_GRACE: Duration = Duration::from_mins(10);

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

    /// The mouse serial, or `None` when it has none worth keying on or does
    /// not answer; neither may fail the session that asked.
    async fn serial_number(&mut self) -> Option<SerialNumber> {
        let response = self.transact(&Request::serial_number()).await.ok()?;
        SerialNumber::decode(&response).ok().flatten()
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

/// The DPI stages stored on the mouse, with the active one.
///
/// # Errors
///
/// [`WriteError`] when the mouse is unreachable or its flash is unreadable.
pub async fn read_stages(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardDpiStages, WriteError> {
    let config = with_link(backend, route, async |link| link.read_config().await).await?;
    Ok(onboard_stages(&config))
}

/// Replace the DPI stages stored on the mouse and activate one, then read
/// them back and return what the mouse reports.
///
/// The list is written so a session cut short never leaves the flash
/// unreadable: a growing list stores its new entries before the count that
/// reveals them, and a shrinking list lowers the active stage and the count
/// before the entries.
///
/// # Errors
///
/// [`WriteError::DpiUnsupported`] for a DPI the flash cannot store,
/// [`WriteError::RawDeviceProtocol`] for an empty or oversized list or an
/// active stage outside it, and [`WriteError::OnboardDpiStagesNotApplied`]
/// when the read-back disagrees.
pub async fn set_stages(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: &OnboardDpiStagesChange,
) -> Result<OnboardDpiStages, WriteError> {
    let stages = change
        .stages
        .iter()
        .map(|stage| {
            let dpi = Dpi::new(stage.dpi.into_inner()).ok_or(WriteError::DpiUnsupported {
                requested: stage.dpi,
            })?;
            Ok(DpiStage {
                x: dpi,
                y: dpi,
                color: device_color(stage.color),
            })
        })
        .collect::<Result<Vec<_>, WriteError>>()?;
    let mut writes = config::set_stages(&stages, change.active)?;
    // The encoder leads with the count and active-stage writes, then the
    // entry tables.
    let entries = writes.split_off(2);
    with_link(backend, route, async |link| {
        let before = link.read_config().await?.stages.len();
        let ordered: Vec<Request> = if stages.len() >= before {
            entries.into_iter().chain(writes).collect()
        } else {
            writes.into_iter().rev().chain(entries).collect()
        };
        for request in &ordered {
            link.transact(request).await?;
        }
        let actual = onboard_stages(&link.read_config().await?);
        if actual.stages == change.stages && actual.active == change.active {
            Ok(actual)
        } else {
            Err(WriteError::OnboardDpiStagesNotApplied)
        }
    })
    .await
}

/// The sensor, timing and lighting settings stored on the mouse.
///
/// # Errors
///
/// [`WriteError`] when the mouse is unreachable or its flash is unreadable.
pub async fn read_settings(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<MouseSettings, WriteError> {
    let config = with_link(backend, route, async |link| link.read_config().await).await?;
    Ok(config.settings)
}

/// Apply one change to the mouse's settings, then read them back and return
/// what the mouse reports.
///
/// Every value in a [`MouseSettingsChange`] is in range by construction, so
/// nothing is refused before the session opens.
///
/// # Errors
///
/// [`WriteError::MouseSettingNotApplied`] when the read-back does not show the
/// change.
pub async fn set_settings(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: &MouseSettingsChange,
) -> Result<MouseSettings, WriteError> {
    let writes = config::change_requests(change)?;
    with_link(backend, route, async |link| {
        for request in &writes {
            link.transact(request).await?;
        }
        let actual = link.read_config().await?.settings;
        if actual.includes(change) {
            Ok(actual)
        } else {
            Err(WriteError::MouseSettingNotApplied)
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

/// What the last answered refresh learned about one Compx mouse.
///
/// The caller owns these between refreshes — the agent keeps them in its
/// standalone-node ledger next to the last-good device descriptors — and
/// hands them back to [`refresh_standalone`]. That is what lets a refresh
/// skip the mouse while its reading is fresh, read only the battery once the
/// controls are verified, and replay a briefly silent mouse's last-good
/// state instead of dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMouse {
    route: DeviceRoute,
    battery: BatteryInfo,
    serial: Option<SerialNumber>,
    checked_at: Instant,
    read_at: Instant,
}

impl KnownMouse {
    /// The route of the mouse these readings belong to.
    #[must_use]
    pub fn route(&self) -> &DeviceRoute {
        &self.route
    }

    fn answered(self, battery: Battery, now: Instant) -> Self {
        Self {
            battery: battery_info(battery),
            checked_at: now,
            read_at: now,
            ..self
        }
    }

    /// The state after a check the mouse did not answer: replayed while the
    /// last answer is younger than [`LAST_GOOD_GRACE`], gone after.
    fn unanswered(self, now: Instant) -> Option<Self> {
        (now.saturating_duration_since(self.read_at) < LAST_GOOD_GRACE).then_some(Self {
            checked_at: now,
            ..self
        })
    }

    fn is_fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.checked_at) < CHECK_INTERVAL
    }

    fn apply(&self, device: &mut StandaloneDevice) {
        device.battery = Some(self.battery.clone());
        if let Some(serial) = self.serial {
            device.serial_number = Some(serial.to_string());
        }
        let capabilities = device.capabilities.get_or_insert_default();
        capabilities.pointer = true;
        capabilities.report_rate = true;
        capabilities.onboard_dpi_stages = true;
        capabilities.mouse_settings = true;
    }
}

/// Fill the live fields of every Compx device in `devices`: battery, the DPI
/// and polling-rate capabilities once its configuration reads, and the
/// physical identity once its serial reads. Returns what is now known about
/// each mouse, to pass back as `known` on the next call; a mouse absent from
/// `devices` is absent from the result, so a vanished receiver is gone, not
/// last-good.
///
/// A mouse first seen gets one full session (battery, flash, serial). After
/// that it is checked at most every two minutes, with a battery-only session,
/// and a mouse that does not answer — asleep, switched off, or out of range —
/// keeps its last-good state for ten minutes.
pub async fn refresh_standalone(
    backend: &dyn HidBackend,
    devices: &mut [StandaloneDevice],
    known: &[KnownMouse],
) -> Vec<KnownMouse> {
    let mut refreshed = Vec::new();
    for device in devices
        .iter_mut()
        .filter(|device| is_compx_route(&device.route()))
    {
        let route = device.route();
        let now = Instant::now();
        let prior = known.iter().find(|mouse| mouse.route == route).cloned();
        let mouse = match prior {
            Some(prior) if prior.is_fresh(now) => Some(prior),
            Some(prior) => {
                match with_link(backend, &route, async |link| link.battery().await).await {
                    Ok(battery) => Some(prior.answered(battery, now)),
                    Err(error) => {
                        debug!(%error, "Compx mouse did not report its battery");
                        prior.unanswered(now)
                    }
                }
            }
            None => {
                let first = with_link(backend, &route, async |link| {
                    let battery = link.battery().await?;
                    link.read_config().await?;
                    Ok((battery, link.serial_number().await))
                })
                .await;
                match first {
                    Ok((battery, serial)) => Some(KnownMouse {
                        route: route.clone(),
                        battery: battery_info(battery),
                        serial,
                        checked_at: now,
                        read_at: now,
                    }),
                    Err(error) => {
                        debug!(%error, "Compx mouse did not report its state");
                        None
                    }
                }
            }
        };
        if let Some(mouse) = mouse {
            mouse.apply(device);
            refreshed.push(mouse);
        }
    }
    refreshed
}

fn dpi_info(config: &MouseConfig) -> Result<DpiInfo, WriteError> {
    Ok(DpiInfo {
        current: WireDpi::new(config.current_stage().x.get()),
        capabilities: DpiCapabilities::new(config::storable_dpi().map(Dpi::get).collect())?,
    })
}

fn onboard_stages(config: &MouseConfig) -> OnboardDpiStages {
    OnboardDpiStages {
        stages: config
            .stages
            .iter()
            .map(|stage| OnboardDpiStage {
                dpi: WireDpi::new(stage.x.get()),
                color: WireRgb::new(stage.color.r, stage.color.g, stage.color.b),
            })
            .collect(),
        active: config.active_stage,
        max_stages: u8::try_from(config::MAX_DPI_STAGES).unwrap_or(u8::MAX),
    }
}

fn device_color(color: WireRgb) -> Rgb {
    let (r, g, b) = color.components();
    Rgb { r, g, b }
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
