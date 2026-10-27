//! The sensor, timing and lighting settings a mouse stores in its own memory.
//!
//! A mouse with `Capabilities::mouse_settings` is the only owner of these
//! settings: nothing here is saved in `config.toml`. The state holds the last
//! settings the mouse reported, with the user's edits laid over them while
//! their writes are in flight.
//!
//! An edit shows at once and is written as one change. Writes are answered in
//! the order they were sent, so only the newest answer's read-back replaces the
//! shown settings; a refusal sends the state back to the mouse for the truth
//! instead of leaving the optimistic value standing. A failed read is kept so
//! the panel can offer a retry instead of reading forever.

use openlogi_core::hid::{MouseSettings, MouseSettingsChange, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device stores sensor, timing and lighting settings.
    #[must_use]
    pub fn current_mouse_settings_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.mouse_settings)
    }

    /// The settings the active device last reported, once they are known.
    #[must_use]
    pub fn mouse_settings(&self) -> Option<&MouseSettings> {
        let key = self.current_record()?.device_key();
        self.pointer.mouse_settings.get(&key)
    }

    /// Whether the active device's last settings read failed and none is
    /// known, so the panel should offer a retry.
    #[must_use]
    pub fn mouse_settings_read_failed(&self) -> bool {
        self.current_record().is_some_and(|record| {
            let key = record.device_key();
            self.pointer.failed_mouse_setting_reads.contains(&key)
                && !self.pointer.mouse_settings.contains_key(&key)
        })
    }

    /// Ask the active device for its settings, unless they are known, a read
    /// is already out, or the last read failed and waits for a retry.
    pub(super) fn load_current_mouse_settings(&mut self) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.mouse_settings)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        if self.pointer.mouse_settings.contains_key(&key)
            || self.pointer.failed_mouse_setting_reads.contains(&key)
        {
            return;
        }
        self.request_mouse_settings(key, route);
    }

    /// Read `key`'s settings again after a failed read — the "click to retry"
    /// affordance.
    pub fn retry_mouse_settings_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.failed_mouse_setting_reads.remove(key);
        let route = self
            .devices
            .records
            .iter()
            .find(|record| record.device_key() == *key)
            .and_then(|record| record.route.clone());
        if let Some(route) = route {
            self.request_mouse_settings(key.clone(), route);
        }
        StateEvent::MouseSettingsChanged(key.clone()).into()
    }

    fn request_mouse_settings(&mut self, key: DeviceKey, route: openlogi_core::hid::DeviceRoute) {
        if self.pointer.mouse_setting_reads.insert(key.clone()) {
            self.send_ipc(crate::services::ipc::ReadMouseSettings { route, key });
        }
    }

    /// Show `change` on the active device at once and write it.
    pub fn commit_mouse_setting(&mut self, change: MouseSettingsChange) -> StateEvents {
        let Some(record) = self.current_record() else {
            return StateEvents::default();
        };
        let key = record.device_key();
        let events: StateEvents = StateEvent::MouseSettingsChanged(key.clone()).into();
        let Some(route) = record.route.clone() else {
            return events;
        };
        let Some(settings) = self.pointer.mouse_settings.get(&key).copied() else {
            return events;
        };
        self.pointer.unresponsive_mice.remove(&key);
        *self
            .pointer
            .pending_mouse_setting_writes
            .entry(key.clone())
            .or_default() += 1;
        self.pointer
            .mouse_settings
            .insert(key.clone(), settings.applying(&change));
        self.send_ipc(crate::services::ipc::SetMouseSettings { route, change, key });
        events
    }

    /// The mouse's answer to a settings read.
    pub fn apply_mouse_settings_read(
        &mut self,
        key: &DeviceKey,
        result: Result<MouseSettings, WriteError>,
    ) -> StateEvents {
        self.pointer.mouse_setting_reads.remove(key);
        match result {
            Ok(settings) => {
                self.pointer.unresponsive_mice.remove(key);
                self.pointer.failed_mouse_setting_reads.remove(key);
                if self.pointer.pending_mouse_setting_writes.contains_key(key) {
                    debug!(key = %key, "settings read overtaken by a write — keeping the edit");
                } else {
                    self.pointer.mouse_settings.insert(key.clone(), settings);
                }
            }
            Err(error) => {
                if matches!(error, WriteError::RawDeviceUnresponsive) {
                    self.pointer.unresponsive_mice.insert(key.clone());
                }
                warn!(%error, key = %key, "could not read the mouse's settings");
                self.pointer.failed_mouse_setting_reads.insert(key.clone());
                self.pointer.mouse_settings.remove(key);
            }
        }
        StateEvent::MouseSettingsChanged(key.clone()).into()
    }

    /// The mouse's answer to a settings write: the newest answer shows the
    /// read-back, a refusal or a mouse that could not be reached reads the
    /// settings again so the optimistic value does not stand in for its own.
    pub fn apply_mouse_settings_written(
        &mut self,
        key: &DeviceKey,
        result: Result<MouseSettings, WriteError>,
    ) -> StateEvents {
        let newest = match self.pointer.pending_mouse_setting_writes.get_mut(key) {
            Some(pending) if *pending > 1 => {
                *pending -= 1;
                false
            }
            _ => {
                self.pointer.pending_mouse_setting_writes.remove(key);
                true
            }
        };
        match result {
            Ok(settings) => {
                self.pointer.unresponsive_mice.remove(key);
                if newest {
                    self.pointer.mouse_settings.insert(key.clone(), settings);
                }
            }
            Err(error) => {
                if matches!(error, WriteError::RawDeviceUnresponsive) {
                    self.pointer.unresponsive_mice.insert(key.clone());
                }
                warn!(%error, key = %key, "mouse setting write did not land — re-reading the mouse");
                if newest {
                    self.reread_mouse_settings(key);
                }
            }
        }
        StateEvent::MouseSettingsChanged(key.clone()).into()
    }

    fn reread_mouse_settings(&mut self, key: &DeviceKey) {
        let route = self
            .devices
            .records
            .iter()
            .find(|record| record.device_key() == *key)
            .and_then(|record| record.route.clone());
        if let Some(route) = route {
            self.request_mouse_settings(key.clone(), route);
        }
    }
}
