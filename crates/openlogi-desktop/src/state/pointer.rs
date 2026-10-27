//! Pointer-device query and active-editor state.

use std::collections::{HashMap, HashSet};

use openlogi_core::hid::{Dpi, MouseSettings, OnboardDpiStages};
use swr_core::{Runtime, SwrClient};

use crate::services::device_reads::DeviceReads;

use super::device_key::DeviceKey;
use super::{AppState, DEFAULT_DPI};

pub(super) struct PointerState {
    pub(super) dpi: Dpi,
    pub(super) reads: DeviceReads,
    pub(super) next_smartshift_write_id: u64,
    /// The newest DPI write sent to each device whose answer has not arrived
    /// yet. Only that write's answer may change the shown DPI.
    pub(super) pending_dpi_writes: HashMap<DeviceKey, u64>,
    pub(super) next_dpi_write: u64,
    /// The mice behind a receiver that did not answer their last DPI or
    /// report-rate write with `RawDeviceUnresponsive`. Cleared by the next write
    /// to that mouse.
    pub(super) unresponsive_mice: HashSet<DeviceKey>,
    /// The mice whose last pointer-speed write the mouse refused or the agent
    /// could not deliver.
    pub(super) refused_pointer_speeds: HashSet<DeviceKey>,
    /// The devices whose last press-force write the device refused or the
    /// agent could not deliver.
    pub(super) refused_force_sensing: HashSet<DeviceKey>,
    /// The newest press-force write sent to each device whose answer has not
    /// arrived yet. Only that write's answer may change the shown force or
    /// save it.
    pub(super) pending_force_sensing_writes: HashMap<DeviceKey, u64>,
    pub(super) next_force_sensing_write: u64,
    /// DPI presets of devices with a transient identity, which have no config
    /// entry to hold them. They last until the app quits.
    pub(super) session_dpi_presets: HashMap<DeviceKey, Vec<Dpi>>,
    /// The DPI stages each onboard-stage mouse last reported, or the list the
    /// user just asked it to store.
    pub(super) onboard_stages: HashMap<DeviceKey, OnboardDpiStages>,
    /// The mice with a stage read in flight.
    pub(super) onboard_stage_reads: HashSet<DeviceKey>,
    /// How many stage writes each mouse has yet to answer.
    pub(super) pending_onboard_stage_writes: HashMap<DeviceKey, u32>,
    /// The mice whose last stage read failed and wait for a retry.
    pub(super) failed_onboard_stage_reads: HashSet<DeviceKey>,
    /// The sensor, timing and lighting settings each mouse last reported, or
    /// the settings with the user's edits laid over them.
    pub(super) mouse_settings: HashMap<DeviceKey, MouseSettings>,
    /// The mice with a settings read in flight.
    pub(super) mouse_setting_reads: HashSet<DeviceKey>,
    /// The mice whose last settings read failed and wait for a retry.
    pub(super) failed_mouse_setting_reads: HashSet<DeviceKey>,
    /// How many settings writes each mouse has yet to answer.
    pub(super) pending_mouse_setting_writes: HashMap<DeviceKey, u32>,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            dpi: DEFAULT_DPI,
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
            pending_dpi_writes: HashMap::new(),
            next_dpi_write: 0,
            unresponsive_mice: HashSet::new(),
            refused_pointer_speeds: HashSet::new(),
            refused_force_sensing: HashSet::new(),
            pending_force_sensing_writes: HashMap::new(),
            next_force_sensing_write: 0,
            session_dpi_presets: HashMap::new(),
            onboard_stages: HashMap::new(),
            onboard_stage_reads: HashSet::new(),
            pending_onboard_stage_writes: HashMap::new(),
            failed_onboard_stage_reads: HashSet::new(),
            mouse_settings: HashMap::new(),
            mouse_setting_reads: HashSet::new(),
            failed_mouse_setting_reads: HashSet::new(),
            pending_mouse_setting_writes: HashMap::new(),
        }
    }
}

impl AppState {
    pub(crate) fn connect_device_reads(
        &mut self,
        client: SwrClient,
        runtime: std::sync::Arc<dyn Runtime>,
    ) {
        self.pointer.reads.connect(client.clone(), runtime.clone());
        self.disable_keys_reads.connect(client, runtime);
    }
}
