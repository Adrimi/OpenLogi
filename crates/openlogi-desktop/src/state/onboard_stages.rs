//! The DPI stages a mouse stores in its own memory.
//!
//! A mouse with `Capabilities::onboard_dpi_stages` keeps the stage list its DPI
//! button steps through, and the colour its LED shows for each. The mouse is
//! the only owner of that list: nothing here is saved in `config.toml`, and the
//! host-side presets stay out of the way. The state holds the last list the
//! mouse reported, or the one the user just asked for while the write is in
//! flight.
//!
//! An edit shows at once and is written as one replacement list. Writes are
//! answered in the order they were sent, so only the newest answer's read-back
//! replaces the shown list; a refusal sends the state back to the mouse for the
//! truth instead of leaving the optimistic list standing.

use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    Dpi, OnboardDpiStage, OnboardDpiStages, OnboardDpiStagesChange, WriteError,
};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::{AppState, StateEvent};

/// The colours a new stage takes in turn: the vendor app's default palette.
pub(crate) const STAGE_COLORS: [Rgb; 8] = [
    Rgb::new(0xff, 0x00, 0x00),
    Rgb::new(0x00, 0xff, 0x00),
    Rgb::new(0x00, 0x00, 0xff),
    Rgb::new(0xff, 0xff, 0x00),
    Rgb::new(0xff, 0x80, 0x00),
    Rgb::new(0x00, 0xff, 0xff),
    Rgb::new(0xff, 0x00, 0xff),
    Rgb::new(0xff, 0xff, 0xff),
];

impl AppState {
    /// Whether the active device stores DPI stages of its own, so the DPI
    /// panel edits those instead of host-side presets.
    #[must_use]
    pub fn current_onboard_stages_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.onboard_dpi_stages)
    }

    /// The stages the active device last reported, once they are known.
    #[must_use]
    pub fn onboard_dpi_stages(&self) -> Option<&OnboardDpiStages> {
        let key = self.current_record()?.device_key();
        self.pointer.onboard_stages.get(&key)
    }

    /// Ask the active device for its stages, unless they are known or a read
    /// is already out.
    pub(super) fn load_current_onboard_stages(&mut self) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.onboard_dpi_stages)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        if self.pointer.onboard_stages.contains_key(&key)
            || self.pointer.failed_onboard_stage_reads.contains(&key)
            || !self.pointer.onboard_stage_reads.insert(key.clone())
        {
            return;
        }
        self.send_ipc(crate::services::ipc::ReadOnboardDpiStages { route, key });
    }

    /// Whether the active device's last stage read failed and no list is
    /// known, so the panel should offer a retry.
    #[must_use]
    pub fn onboard_stages_read_failed(&self) -> bool {
        self.current_record().is_some_and(|record| {
            let key = record.device_key();
            self.pointer.failed_onboard_stage_reads.contains(&key)
                && !self.pointer.onboard_stages.contains_key(&key)
        })
    }

    /// Read `key`'s stages again after a failed read — the "click to retry"
    /// affordance.
    pub fn retry_onboard_stages_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.failed_onboard_stage_reads.remove(key);
        self.reread_onboard_stages(key);
        StateEvent::DpiChanged(key.clone()).into()
    }

    /// Make stage `index` the active one.
    pub fn select_onboard_stage(&mut self, index: u8) -> StateEvents {
        self.edit_onboard_stages(|stages| {
            (usize::from(index) < stages.stages.len() && index != stages.active).then(|| {
                OnboardDpiStagesChange {
                    stages: stages.stages.clone(),
                    active: index,
                }
            })
        })
    }

    /// Drop stage `index`, keeping at least one. The active stage stays the
    /// same one unless it was the dropped stage, which hands over to the
    /// stage that took its place.
    pub fn remove_onboard_stage(&mut self, index: u8) -> StateEvents {
        self.edit_onboard_stages(|stages| {
            let position = usize::from(index);
            if stages.stages.len() <= 1 || position >= stages.stages.len() {
                return None;
            }
            let mut remaining = stages.stages.clone();
            remaining.remove(position);
            let active = if stages.active > index {
                stages.active - 1
            } else {
                stages.active
            };
            let last = u8::try_from(remaining.len() - 1).unwrap_or(u8::MAX);
            Some(OnboardDpiStagesChange {
                stages: remaining,
                active: active.min(last),
            })
        })
    }

    /// Append the DPI the editor shows as a new stage, in the next colour of
    /// [`STAGE_COLORS`]. The active stage stays as it is.
    pub fn add_onboard_stage(&mut self) -> StateEvents {
        let dpi = self.normalize_active_dpi(self.dpi());
        self.edit_onboard_stages(|stages| {
            (stages.stages.len() < usize::from(stages.max_stages)).then(|| {
                let mut grown = stages.stages.clone();
                grown.push(OnboardDpiStage {
                    dpi,
                    color: next_stage_color(&grown),
                });
                OnboardDpiStagesChange {
                    stages: grown,
                    active: stages.active,
                }
            })
        })
    }

    /// Apply the change `edit` derives from the active device's stages, show
    /// it, and write it. No-op when there is no change to make.
    fn edit_onboard_stages(
        &mut self,
        edit: impl FnOnce(&OnboardDpiStages) -> Option<OnboardDpiStagesChange>,
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::DpiChanged);
        let Some(record) = self.current_record() else {
            return events;
        };
        let key = record.device_key();
        let Some(route) = record.route.clone() else {
            return events;
        };
        let Some(stages) = self.pointer.onboard_stages.get(&key) else {
            return events;
        };
        let max_stages = stages.max_stages;
        let Some(change) = edit(stages) else {
            return events;
        };
        self.pointer.unresponsive_mice.remove(&key);
        *self
            .pointer
            .pending_onboard_stage_writes
            .entry(key.clone())
            .or_default() += 1;
        self.show_onboard_stages(
            &key,
            OnboardDpiStages {
                stages: change.stages.clone(),
                active: change.active,
                max_stages,
            },
        );
        self.send_ipc(crate::services::ipc::SetOnboardDpiStages { route, change, key });
        events
    }

    /// The mouse's answer to a stage read.
    pub fn apply_onboard_stages_read(
        &mut self,
        key: &DeviceKey,
        result: Result<OnboardDpiStages, WriteError>,
    ) -> StateEvents {
        self.pointer.onboard_stage_reads.remove(key);
        match result {
            Ok(stages) => {
                self.pointer.unresponsive_mice.remove(key);
                self.pointer.failed_onboard_stage_reads.remove(key);
                if self.pointer.pending_onboard_stage_writes.contains_key(key) {
                    debug!(key = %key, "stage read overtaken by a write — keeping the write's list");
                } else {
                    self.show_onboard_stages(key, stages);
                }
            }
            Err(error) => {
                if matches!(error, WriteError::RawDeviceUnresponsive) {
                    self.pointer.unresponsive_mice.insert(key.clone());
                }
                warn!(%error, key = %key, "could not read the mouse's DPI stages");
                self.pointer.failed_onboard_stage_reads.insert(key.clone());
                self.pointer.onboard_stages.remove(key);
            }
        }
        StateEvent::DpiChanged(key.clone()).into()
    }

    /// The mouse's answer to a stage write: the newest answer shows the
    /// read-back, a refusal or a mouse that could not be reached reads the
    /// stages again so the optimistic list does not stand in for its own.
    pub fn apply_onboard_stages_written(
        &mut self,
        key: &DeviceKey,
        result: Result<OnboardDpiStages, WriteError>,
    ) -> StateEvents {
        let newest = match self.pointer.pending_onboard_stage_writes.get_mut(key) {
            Some(pending) if *pending > 1 => {
                *pending -= 1;
                false
            }
            _ => {
                self.pointer.pending_onboard_stage_writes.remove(key);
                true
            }
        };
        match result {
            Ok(stages) => {
                self.pointer.unresponsive_mice.remove(key);
                if newest {
                    self.show_onboard_stages(key, stages);
                }
            }
            Err(error) => {
                if matches!(error, WriteError::RawDeviceUnresponsive) {
                    self.pointer.unresponsive_mice.insert(key.clone());
                }
                warn!(%error, key = %key, "DPI stage write did not land — re-reading the mouse");
                if newest {
                    self.reread_onboard_stages(key);
                }
            }
        }
        StateEvent::DpiChanged(key.clone()).into()
    }

    fn reread_onboard_stages(&mut self, key: &DeviceKey) {
        let Some(route) = self
            .devices
            .records
            .iter()
            .find(|record| record.device_key() == *key)
            .and_then(|record| record.route.clone())
        else {
            return;
        };
        if self.pointer.onboard_stage_reads.insert(key.clone()) {
            self.send_ipc(crate::services::ipc::ReadOnboardDpiStages {
                route,
                key: key.clone(),
            });
        }
    }

    /// Keep the stored active stage in step with a DPI the editor's slider
    /// just wrote to the mouse, unless a stage write is still in flight.
    pub(super) fn sync_onboard_active_dpi(&mut self, key: &DeviceKey, dpi: Dpi) {
        if self.pointer.pending_onboard_stage_writes.contains_key(key) {
            return;
        }
        if let Some(stages) = self.pointer.onboard_stages.get_mut(key)
            && let Some(active) = stages.stages.get_mut(usize::from(stages.active))
        {
            active.dpi = dpi;
        }
    }

    /// Store `stages` for `key`, and when it is the active device make the
    /// editor and the DPI reading show the active stage's DPI.
    fn show_onboard_stages(&mut self, key: &DeviceKey, stages: OnboardDpiStages) {
        let active = stages.active_stage().map(|stage| stage.dpi);
        self.pointer.onboard_stages.insert(key.clone(), stages);
        if let Some(dpi) = active {
            self.pointer.reads.set_dpi_current(key, dpi);
            if self.is_current_device(key) {
                self.pointer.dpi = dpi;
            }
        }
    }
}

/// The first colour of [`STAGE_COLORS`] no stage uses, else the one the next
/// position falls on.
fn next_stage_color(stages: &[OnboardDpiStage]) -> Rgb {
    STAGE_COLORS
        .into_iter()
        .find(|color| stages.iter().all(|stage| stage.color != *color))
        .unwrap_or(STAGE_COLORS[stages.len() % STAGE_COLORS.len()])
}
