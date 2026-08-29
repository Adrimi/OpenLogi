//! HID++ `Backlight` (feature `0x1982`) — keyboard backlight control.
//!
//! The protocol-level `0x1982` wrapper lives in `openlogi-hidpp`; this module
//! keeps OpenLogi's IPC/config-facing mode, status, and snapshot types.
//!
//! This is the backlight family used by the MX Keys line: a white,
//! level-adjustable backlight driven by an ambient-light sensor and a hand
//! proximity sensor. It is distinct from the RGB families (`0x8070`
//! ColorLedEffects, `0x8080` PerKeyLighting) that [`crate::set_keyboard_color`]
//! drives — a device exposes one or the other, never both.
//!
//! `setBacklightConfig` writes to the device's non-volatile memory, so a
//! disabled backlight stays disabled across reconnects, host switches, and
//! power cycles without a daemon re-applying it.

use serde::{Deserialize, Serialize};

/// How the firmware decides the backlight brightness level.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*, so
/// variant order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightMode {
    /// No mode selected.
    None,
    /// Level follows the ambient-light sensor.
    Automatic,
    /// Level adjusted with the keyboard's own backlight keys. The firmware
    /// enters this mode on its own; software cannot write it.
    TemporaryManual,
    /// Level set by software and held until changed.
    PermanentManual,
}

/// Why the backlight is in its current state, as reported by
/// `getBacklightInfo`.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*, so
/// variant order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightStatus {
    /// Turned off by software — the LEDs stay dark regardless of ambient
    /// light or hand proximity. This is what [`crate::set_backlight_enabled`]
    /// with `false` produces.
    DisabledBySoftware,
    /// Turned off because the battery is critically low.
    DisabledByCriticalBattery,
    /// Following the ambient-light sensor.
    AlsAutomatic,
    /// Following the ambient-light sensor, which reads bright enough that the
    /// LEDs are off.
    AlsSaturated,
    /// Holding a level the user picked with the backlight keys.
    TemporaryManual,
    /// Holding a level written by software.
    PermanentManual,
}

/// Snapshot of a keyboard's backlight, merged from the `0x1982`
/// `getBacklightConfig` and `getBacklightInfo` responses.
///
/// Crosses the agent↔GUI IPC, so field order is wire format — changes require
/// a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklightState {
    /// Whether the backlight system is enabled at all. When `false` the
    /// firmware keeps the LEDs dark no matter what the sensors report, and
    /// [`Self::status`] reads [`BacklightStatus::DisabledBySoftware`].
    pub enabled: bool,
    /// How the level is chosen while the backlight is enabled.
    pub mode: BacklightMode,
    /// Why the backlight is in its current state.
    pub status: BacklightStatus,
    /// Current brightness level, `0` (off) up to [`Self::nb_levels`] minus one.
    pub current_level: u8,
    /// Number of user-selectable brightness levels the device reports.
    pub nb_levels: u8,
}

/// Which way a backlight step moves the level.
///
/// Local to the write path — this never crosses the agent↔GUI IPC, so it
/// carries no wire-format constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklightStep {
    /// One level brighter.
    Up,
    /// One level dimmer.
    Down,
}

impl BacklightState {
    /// The level one `step` away, or `None` when the backlight already sits at
    /// that end of its range.
    ///
    /// `None` is what lets the caller skip the write entirely, which matters
    /// here: `setBacklightConfig` writes non-volatile memory, so holding a
    /// backlight key at the limit must not keep rewriting it.
    ///
    /// [`Self::nb_levels`] is a count, so the brightest selectable level is one
    /// below it. A firmware reporting a level past that end is clamped back
    /// into range rather than trusted, so one step always lands on a level the
    /// device will accept.
    #[must_use]
    pub fn stepped_level(self, step: BacklightStep) -> Option<u8> {
        let brightest = self.nb_levels.checked_sub(1)?;
        let current = self.current_level.min(brightest);
        match step {
            BacklightStep::Up => (current < brightest).then_some(current + 1),
            BacklightStep::Down => current.checked_sub(1),
        }
    }

    /// Whether the LEDs are dark right now, for whatever reason — software
    /// disable, critical battery, a saturated ambient-light sensor, or a zero
    /// manual level.
    #[must_use]
    pub fn is_dark(self) -> bool {
        !self.enabled
            || self.current_level == 0
            || matches!(
                self.status,
                BacklightStatus::DisabledBySoftware
                    | BacklightStatus::DisabledByCriticalBattery
                    | BacklightStatus::AlsSaturated
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit() -> BacklightState {
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 4,
            nb_levels: 8,
        }
    }

    #[test]
    fn a_lit_backlight_is_not_dark() {
        assert!(!lit().is_dark());
    }

    #[test]
    fn software_disable_reads_as_dark() {
        let state = BacklightState {
            enabled: false,
            status: BacklightStatus::DisabledBySoftware,
            ..lit()
        };
        assert!(state.is_dark());
    }

    #[test]
    fn a_zero_level_reads_as_dark_even_while_enabled() {
        let state = BacklightState {
            current_level: 0,
            ..lit()
        };
        assert!(state.is_dark());
    }

    #[test]
    fn a_saturated_ambient_sensor_reads_as_dark() {
        let state = BacklightState {
            status: BacklightStatus::AlsSaturated,
            ..lit()
        };
        assert!(state.is_dark());
    }

    #[test]
    fn a_step_moves_one_level_in_the_requested_direction() {
        assert_eq!(lit().stepped_level(BacklightStep::Up), Some(5));
        assert_eq!(lit().stepped_level(BacklightStep::Down), Some(3));
    }

    #[test]
    fn a_step_stops_at_each_end_instead_of_wrapping() {
        let brightest = BacklightState {
            current_level: 7,
            ..lit()
        };
        assert_eq!(brightest.stepped_level(BacklightStep::Up), None);
        assert_eq!(brightest.stepped_level(BacklightStep::Down), Some(6));

        let off = BacklightState {
            current_level: 0,
            ..lit()
        };
        assert_eq!(off.stepped_level(BacklightStep::Down), None);
        assert_eq!(off.stepped_level(BacklightStep::Up), Some(1));
    }

    #[test]
    fn a_device_offering_no_levels_cannot_be_stepped() {
        let state = BacklightState {
            nb_levels: 0,
            current_level: 0,
            ..lit()
        };
        assert_eq!(state.stepped_level(BacklightStep::Up), None);
        assert_eq!(state.stepped_level(BacklightStep::Down), None);
    }

    #[test]
    fn a_level_past_the_reported_range_is_clamped_not_trusted() {
        let state = BacklightState {
            current_level: 9,
            ..lit()
        };
        assert_eq!(
            state.stepped_level(BacklightStep::Up),
            None,
            "9 clamps to the brightest level, which cannot go higher"
        );
        assert_eq!(state.stepped_level(BacklightStep::Down), Some(6));
    }
}
