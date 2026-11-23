use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::async_trait;
use hidpp::channel::HidppChannel;
use openlogi_core::color::Rgb;
use openlogi_core::device::{BatteryLevel, BatteryStatus, StandaloneDevice};
use openlogi_core::hid::dpi::Dpi;
use openlogi_core::hid::report_rate::ReportRate;
use openlogi_core::hid::{
    DeviceRoute, LiftOffDistance, MouseSettingsChange, OnboardDpiStage, OnboardDpiStages,
    OnboardDpiStagesChange, WriteError,
};

use super::{
    CHECK_INTERVAL, KnownMouse, LAST_GOOD_GRACE, read_dpi_info, read_report_rate_info,
    read_settings, read_stages, refresh_standalone, set_dpi, set_report_rate, set_settings,
    set_stages,
};
use crate::backend::{
    BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawDuplex, RawWriter,
};
use crate::compx::config::tests::dump;
use crate::compx::{REPORT_LEN, checksum};
use crate::inventory::standalone::enumerate_standalone;

const SERIAL: [u8; 10] = [0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x00, 0x01];

/// How the simulated mouse treats requests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Answers like the captured hardware.
    Awake,
    /// Asleep: the receiver relays nothing back.
    Asleep,
    /// Acknowledges flash writes but keeps its flash unchanged.
    IgnoresWrites,
}

/// A Compx receiver and mouse that answer from a flash image.
struct Mouse {
    behaviour: Behaviour,
    flash: Vec<u8>,
    attached: bool,
    pending: VecDeque<[u8; REPORT_LEN]>,
    requests: Vec<u8>,
    written: Vec<usize>,
}

impl Mouse {
    fn reply(&mut self, request: &[u8]) {
        let opcode = request[1];
        self.requests.push(opcode);
        if self.behaviour == Behaviour::Asleep {
            return;
        }
        let mut reply = [0u8; REPORT_LEN];
        reply[..6].copy_from_slice(&request[..6]);
        let address = usize::from(u16::from_be_bytes([request[3], request[4]]));
        let length = usize::from(request[5]);
        match opcode {
            0x02 => {
                self.attached = request[6] == 1;
                reply[6] = request[6];
            }
            // Nothing beyond the handshake answers until a driver attaches.
            _ if !self.attached => return,
            0x04 => {
                reply[5] = 2;
                reply[6..10].copy_from_slice(&[0x14, 0x00, 0x0e, 0x1c]);
            }
            0x20 => {
                reply[5] = 10;
                reply[6..16].copy_from_slice(&SERIAL);
            }
            0x08 => reply[6..6 + length].copy_from_slice(&self.flash[address..address + length]),
            0x07 => {
                self.written.push(address);
                if self.behaviour != Behaviour::IgnoresWrites {
                    self.flash[address..address + length].copy_from_slice(&request[6..6 + length]);
                }
            }
            _ => reply[2] = 0x01,
        }
        reply[16] = checksum(&reply[..16]);
        // A stray report for another collection precedes every reply, as a
        // shared collection would deliver one.
        let mut stray = [0u8; REPORT_LEN];
        stray[0] = 0x01;
        self.pending.push_back(stray);
        self.pending.push_back(reply);
    }
}

#[derive(Clone)]
struct MouseBackend {
    mouse: Arc<Mutex<Mouse>>,
}

impl MouseBackend {
    fn new(behaviour: Behaviour) -> Self {
        Self {
            mouse: Arc::new(Mutex::new(Mouse {
                behaviour,
                flash: dump(),
                attached: false,
                pending: VecDeque::new(),
                requests: Vec::new(),
                written: Vec::new(),
            })),
        }
    }

    fn with<T>(&self, body: impl FnOnce(&mut Mouse) -> T) -> T {
        body(&mut self.mouse.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn node() -> NodeInfo {
        NodeInfo {
            id: NodeId::from("compx-node".to_owned()),
            vendor_id: 0x3554,
            product_id: 0xf524,
            usage_page: 0xff05,
            usage_id: 0x0000,
            name: "2.4G Wireless Receiver".to_owned(),
            manufacturer: Some("Compx".to_owned()),
            serial_number: None,
        }
    }
}

struct MouseDuplex(MouseBackend);

#[async_trait]
impl RawDuplex for MouseDuplex {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        self.0.with(|mouse| mouse.reply(report));
        Ok(())
    }

    async fn read_input_report(&mut self, buffer: &mut [u8]) -> Result<usize, BackendError> {
        match self.0.with(|mouse| mouse.pending.pop_front()) {
            Some(report) => {
                buffer[..REPORT_LEN].copy_from_slice(&report);
                Ok(REPORT_LEN)
            }
            None => std::future::pending().await,
        }
    }
}

#[async_trait]
impl HidBackend for MouseBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(vec![Self::node()])
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(Vec::new())
    }

    async fn open_hidpp(
        &self,
        _node: &NodeInfo,
    ) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        Ok(None)
    }

    async fn open_raw_writer(&self, _node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        Err(BackendError::Disconnected)
    }

    async fn open_raw_duplex(&self, _node: &NodeInfo) -> Result<Box<dyn RawDuplex>, BackendError> {
        Ok(Box::new(MouseDuplex(self.clone())))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Err(BackendError::Disconnected)
    }
}

async fn discovered(backend: &MouseBackend) -> Vec<StandaloneDevice> {
    enumerate_standalone(backend).await.expect("enumeration")
}

fn route() -> DeviceRoute {
    DeviceRoute::RawHid {
        vendor_id: 0x3554,
        product_id: 0xf524,
        usage_page: 0xff05,
        usage_id: 0x0000,
        identity: "id:compx-node".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn refresh_reports_battery_and_enables_measured_controls() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let mut devices = discovered(&backend).await;

    refresh_standalone(&backend, &mut devices, &[]).await;

    let battery = devices[0].battery.clone().expect("battery read");
    assert_eq!(battery.percentage, Some(20));
    assert_eq!(battery.level, BatteryLevel::Low);
    assert_eq!(battery.status, BatteryStatus::Discharging);
    let capabilities = devices[0].capabilities.expect("capabilities");
    assert!(
        capabilities.pointer
            && capabilities.report_rate
            && capabilities.onboard_dpi_stages
            && capabilities.mouse_settings
    );
    assert_eq!(
        backend.with(|mouse| (mouse.attached, mouse.requests.last().copied())),
        (false, Some(0x02)),
        "the session releases the driver when it ends"
    );
}

#[tokio::test(start_paused = true)]
async fn a_sleeping_mouse_keeps_no_battery_or_controls() {
    let backend = MouseBackend::new(Behaviour::Asleep);
    let mut devices = discovered(&backend).await;
    let before = devices[0].capabilities;

    refresh_standalone(&backend, &mut devices, &[]).await;

    assert!(devices[0].battery.is_none());
    assert_eq!(devices[0].capabilities, before);
    assert_eq!(
        read_dpi_info(&backend, &route())
            .await
            .map(|info| info.current),
        Err(WriteError::RawDeviceUnresponsive)
    );
}

#[tokio::test(start_paused = true)]
async fn dpi_reads_the_active_stage_and_writes_it_back() {
    let backend = MouseBackend::new(Behaviour::Awake);

    let info = read_dpi_info(&backend, &route()).await.expect("dpi");
    assert_eq!(info.current, Dpi::new(1400));
    assert!(info.capabilities.values().contains(&Dpi::new(12_800)));

    set_dpi(&backend, &route(), Dpi::new(3200))
        .await
        .expect("write");
    let info = read_dpi_info(&backend, &route()).await.expect("dpi");
    assert_eq!(info.current, Dpi::new(3200));
    assert_eq!(
        backend.with(|mouse| mouse.flash[0x18..0x1c].to_vec()),
        vec![0x3f, 0x3f, 0x00, 0x55u8.wrapping_sub(0x7e)],
        "only stage 3's entry changes, with its check byte"
    );
}

#[tokio::test(start_paused = true)]
async fn unstorable_dpi_is_refused_before_any_write() {
    let backend = MouseBackend::new(Behaviour::Awake);

    assert_eq!(
        set_dpi(&backend, &route(), Dpi::new(825)).await,
        Err(WriteError::DpiUnsupported {
            requested: Dpi::new(825)
        })
    );
    assert!(backend.with(|mouse| !mouse.requests.contains(&0x07)));
}

#[tokio::test(start_paused = true)]
async fn an_ignored_write_is_reported_as_not_applied() {
    let backend = MouseBackend::new(Behaviour::IgnoresWrites);

    assert_eq!(
        set_dpi(&backend, &route(), Dpi::new(3200)).await,
        Err(WriteError::DpiNotApplied {
            requested: Dpi::new(3200),
            actual: Dpi::new(1400),
        })
    );
    assert!(matches!(
        set_report_rate(&backend, &route(), ReportRate::new(500)).await,
        Err(WriteError::ReportRateNotApplied { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn report_rate_offers_receiver_rates_and_applies_one() {
    let backend = MouseBackend::new(Behaviour::Awake);

    let info = read_report_rate_info(&backend, &route())
        .await
        .expect("rate");
    assert_eq!(info.current, ReportRate::new(1000));
    assert_eq!(
        info.supported,
        [125, 250, 500, 1000].map(ReportRate::new).to_vec()
    );

    let info = set_report_rate(&backend, &route(), ReportRate::new(500))
        .await
        .expect("write");
    assert_eq!(info.current, ReportRate::new(500));
    assert_eq!(
        set_report_rate(&backend, &route(), ReportRate::new(8000)).await,
        Err(WriteError::ReportRateUnsupported {
            requested: ReportRate::new(8000)
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_route_to_another_device_is_not_found() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let DeviceRoute::RawHid {
        vendor_id,
        product_id,
        usage_page,
        usage_id,
        ..
    } = route()
    else {
        unreachable!()
    };
    let elsewhere = DeviceRoute::RawHid {
        vendor_id,
        product_id,
        usage_page,
        usage_id,
        identity: "id:another-node".into(),
    };

    assert_eq!(
        read_dpi_info(&backend, &elsewhere)
            .await
            .map(|info| info.current),
        Err(WriteError::DeviceNotFound)
    );
}

async fn refreshed(
    backend: &MouseBackend,
    known: &[KnownMouse],
) -> (StandaloneDevice, Vec<KnownMouse>) {
    let mut devices = discovered(backend).await;
    let known = refresh_standalone(backend, &mut devices, known).await;
    (devices.remove(0), known)
}

#[tokio::test(start_paused = true)]
async fn the_first_refresh_reads_the_serial_as_a_physical_identity() {
    let backend = MouseBackend::new(Behaviour::Awake);

    let (device, known) = refreshed(&backend, &[]).await;

    assert_eq!(
        device.serial_number.as_deref(),
        Some("31323334353637380001")
    );
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].route(), &route());
}

#[tokio::test(start_paused = true)]
async fn a_known_mouse_is_not_touched_while_its_reading_is_fresh() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let (_, known) = refreshed(&backend, &[]).await;
    let sent = backend.with(|mouse| mouse.requests.len());

    tokio::time::advance(CHECK_INTERVAL / 2).await;
    let (device, known) = refreshed(&backend, &known).await;

    assert_eq!(backend.with(|mouse| mouse.requests.len()), sent);
    assert_eq!(device.battery.and_then(|b| b.percentage), Some(20));
    assert_eq!(known.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_due_check_reads_only_the_battery() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let (_, known) = refreshed(&backend, &[]).await;
    backend.with(|mouse| mouse.requests.clear());

    tokio::time::advance(CHECK_INTERVAL).await;
    let (device, _) = refreshed(&backend, &known).await;

    assert_eq!(
        backend.with(|mouse| mouse.requests.clone()),
        vec![0x02, 0x04, 0x02],
        "attach, battery, detach and no flash or serial read"
    );
    assert_eq!(device.battery.and_then(|b| b.percentage), Some(20));
}

#[tokio::test(start_paused = true)]
async fn a_silent_mouse_keeps_its_last_good_state_for_a_bounded_grace() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let (_, known) = refreshed(&backend, &[]).await;
    backend.with(|mouse| mouse.behaviour = Behaviour::Asleep);

    tokio::time::advance(CHECK_INTERVAL).await;
    let (device, known) = refreshed(&backend, &known).await;

    assert_eq!(device.battery.and_then(|b| b.percentage), Some(20));
    let capabilities = device.capabilities.expect("capabilities");
    assert!(capabilities.pointer && capabilities.report_rate);
    assert_eq!(
        device.serial_number.as_deref(),
        Some("31323334353637380001")
    );
    assert_eq!(known.len(), 1);

    tokio::time::advance(LAST_GOOD_GRACE).await;
    let (device, known) = refreshed(&backend, &known).await;

    assert!(device.battery.is_none());
    assert!(known.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_mouse_that_answers_again_after_silence_is_refreshed() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let (_, known) = refreshed(&backend, &[]).await;
    backend.with(|mouse| mouse.behaviour = Behaviour::Asleep);
    tokio::time::advance(CHECK_INTERVAL).await;
    let (_, known) = refreshed(&backend, &known).await;
    backend.with(|mouse| mouse.behaviour = Behaviour::Awake);

    tokio::time::advance(CHECK_INTERVAL).await;
    let (device, known) = refreshed(&backend, &known).await;

    assert_eq!(device.battery.and_then(|b| b.percentage), Some(20));
    assert_eq!(known.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_vanished_receiver_leaves_nothing_to_replay() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let (_, known) = refreshed(&backend, &[]).await;

    let mut none: Vec<StandaloneDevice> = Vec::new();
    assert!(
        refresh_standalone(&backend, &mut none, &known)
            .await
            .is_empty()
    );
}

fn stage(dpi: u16, r: u8, g: u8, b: u8) -> OnboardDpiStage {
    OnboardDpiStage {
        dpi: Dpi::new(dpi),
        color: Rgb::new(r, g, b),
    }
}

#[tokio::test(start_paused = true)]
async fn stages_read_the_onboard_list_and_active_stage() {
    let backend = MouseBackend::new(Behaviour::Awake);

    let stages = read_stages(&backend, &route()).await.expect("stages");

    assert_eq!(
        stages,
        OnboardDpiStages {
            stages: vec![
                stage(800, 0xff, 0, 0),
                stage(1000, 0, 0xff, 0),
                stage(1200, 0xff, 0xff, 0),
                stage(1400, 0, 0, 0xff),
                stage(1600, 0xff, 0x80, 0),
            ],
            active: 3,
            max_stages: 8,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_growing_list_stores_its_entries_before_the_count() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let mut stages = read_stages(&backend, &route())
        .await
        .expect("stages")
        .stages;
    stages.push(stage(3200, 0, 0xff, 0xff));
    let change = OnboardDpiStagesChange { stages, active: 5 };

    let applied = set_stages(&backend, &route(), &change)
        .await
        .expect("write");

    assert_eq!(applied.stages, change.stages);
    assert_eq!(applied.active, 5);
    let written = backend.with(|mouse| mouse.written.clone());
    assert_eq!(&written[written.len() - 2..], &[0x02, 0x04]);
    assert!(written[..written.len() - 2].iter().all(|a| *a >= 0x0c));
}

#[tokio::test(start_paused = true)]
async fn a_shrinking_list_lowers_active_and_count_before_the_entries() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let change = OnboardDpiStagesChange {
        stages: vec![stage(400, 1, 2, 3), stage(6400, 4, 5, 6)],
        active: 1,
    };

    let applied = set_stages(&backend, &route(), &change)
        .await
        .expect("write");

    assert_eq!(applied.stages, change.stages);
    assert_eq!(applied.active, 1);
    let written = backend.with(|mouse| mouse.written.clone());
    assert_eq!(&written[..2], &[0x04, 0x02]);
    assert!(written[2..].iter().all(|a| *a >= 0x0c));
    assert_eq!(
        read_dpi_info(&backend, &route()).await.map(|i| i.current),
        Ok(Dpi::new(6400)),
        "the active stage drives the DPI reading"
    );
}

#[tokio::test(start_paused = true)]
async fn invalid_stage_lists_are_refused_before_any_write() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let change = |stages, active| OnboardDpiStagesChange { stages, active };

    assert_eq!(
        set_stages(&backend, &route(), &change(vec![stage(825, 0, 0, 0)], 0)).await,
        Err(WriteError::DpiUnsupported {
            requested: Dpi::new(825)
        })
    );
    assert!(matches!(
        set_stages(&backend, &route(), &change(vec![], 0)).await,
        Err(WriteError::RawDeviceProtocol(_))
    ));
    assert!(matches!(
        set_stages(&backend, &route(), &change(vec![stage(800, 0, 0, 0); 9], 0)).await,
        Err(WriteError::RawDeviceProtocol(_))
    ));
    assert!(matches!(
        set_stages(&backend, &route(), &change(vec![stage(800, 0, 0, 0)], 1)).await,
        Err(WriteError::RawDeviceProtocol(_))
    ));
    assert!(backend.with(|mouse| mouse.requests.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn ignored_stage_writes_are_reported_as_not_applied() {
    let backend = MouseBackend::new(Behaviour::IgnoresWrites);
    let change = OnboardDpiStagesChange {
        stages: vec![stage(800, 0, 0, 0)],
        active: 0,
    };

    assert_eq!(
        set_stages(&backend, &route(), &change).await,
        Err(WriteError::OnboardDpiStagesNotApplied)
    );
}

#[tokio::test(start_paused = true)]
async fn a_sleeping_mouse_cannot_read_or_set_stages() {
    let backend = MouseBackend::new(Behaviour::Asleep);
    let change = OnboardDpiStagesChange {
        stages: vec![stage(800, 0, 0, 0)],
        active: 0,
    };

    assert_eq!(
        read_stages(&backend, &route()).await,
        Err(WriteError::RawDeviceUnresponsive)
    );
    assert_eq!(
        set_stages(&backend, &route(), &change).await,
        Err(WriteError::RawDeviceUnresponsive)
    );
}

#[tokio::test(start_paused = true)]
async fn settings_are_read_from_the_flash() {
    let backend = MouseBackend::new(Behaviour::Awake);

    let settings = read_settings(&backend, &route()).await.expect("settings");

    assert_eq!(settings.lift_off, LiftOffDistance::Mm1);
}

#[tokio::test(start_paused = true)]
async fn a_setting_change_is_written_and_read_back() {
    let backend = MouseBackend::new(Behaviour::Awake);
    let change = MouseSettingsChange::LiftOff(LiftOffDistance::Mm2);

    let applied = set_settings(&backend, &route(), &change)
        .await
        .expect("write");

    assert_eq!(applied.lift_off, LiftOffDistance::Mm2);
    assert_eq!(backend.with(|mouse| mouse.written.clone()), [0x0a]);
}

#[tokio::test(start_paused = true)]
async fn ignored_setting_writes_are_reported_as_not_applied() {
    let backend = MouseBackend::new(Behaviour::IgnoresWrites);
    let change = MouseSettingsChange::LiftOff(LiftOffDistance::Mm2);

    assert_eq!(
        set_settings(&backend, &route(), &change).await,
        Err(WriteError::MouseSettingNotApplied)
    );
}

#[tokio::test(start_paused = true)]
async fn a_sleeping_mouse_cannot_read_or_set_settings() {
    let backend = MouseBackend::new(Behaviour::Asleep);
    let change = MouseSettingsChange::LiftOff(LiftOffDistance::Mm2);

    assert_eq!(
        read_settings(&backend, &route()).await,
        Err(WriteError::RawDeviceUnresponsive)
    );
    assert_eq!(
        set_settings(&backend, &route(), &change).await,
        Err(WriteError::RawDeviceUnresponsive)
    );
}
