//! Sensor, timing and lighting settings a mouse stores in its own memory —
//! pure data, no I/O.
//!
//! The Compx family keeps these next to its DPI stages (see
//! [`super::onboard_dpi`]). [`MouseSettings`] is everything the settings app
//! shows, [`MouseSettingsChange`] one edit of it. Every numeric type refuses an
//! out-of-range value at construction and when it is deserialized, so a change
//! that reaches the driver is always one the mouse can store. The driver that
//! reads and writes them lives in `openlogi_device::compx`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::Rgb;

/// A number outside the range its setting accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("{value} is outside {min}..={max}")]
pub struct OutOfRange {
    /// The rejected value.
    pub value: i16,
    /// Smallest accepted value.
    pub min: i16,
    /// Largest accepted value.
    pub max: i16,
}

/// How far the sensor must be lifted before it stops tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiftOffDistance {
    /// 0.7 mm.
    Mm0Point7,
    /// 1 mm.
    Mm1,
    /// 2 mm.
    Mm2,
}

impl LiftOffDistance {
    /// Every distance, shortest first.
    pub const ALL: [Self; 3] = [Self::Mm0Point7, Self::Mm1, Self::Mm2];
}

/// The sensor's power profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorMode {
    /// Low power: longer battery life.
    LowPower,
    /// High performance: lower latency.
    HighPerformance,
}

impl SensorMode {
    /// Both modes, low power first.
    pub const ALL: [Self; 2] = [Self::LowPower, Self::HighPerformance];
}

/// One on/off sensor option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorSwitch {
    /// Motion sync: align sensor reports to the polling interval.
    MotionSync,
    /// Angle snapping: straighten near-straight lines.
    AngleSnapping,
    /// Ripple control: smooth the path at high DPI.
    RippleControl,
    /// Highest-performance mode: never let the sensor slow down.
    HighestPerformance,
    /// 20K FPS scanning.
    TwentyKFps,
}

impl SensorSwitch {
    /// Every option, in display order.
    pub const ALL: [Self; 5] = [
        Self::MotionSync,
        Self::AngleSnapping,
        Self::RippleControl,
        Self::HighestPerformance,
        Self::TwentyKFps,
    ];

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which [`SensorSwitch`] options are on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SensorSwitches(u8);

impl SensorSwitches {
    /// Whether `switch` is on.
    #[must_use]
    pub const fn is_on(self, switch: SensorSwitch) -> bool {
        self.0 & switch.bit() != 0
    }

    /// These options with `switch` turned on or off.
    #[must_use]
    pub const fn with(self, switch: SensorSwitch, on: bool) -> Self {
        if on {
            Self(self.0 | switch.bit())
        } else {
            Self(self.0 & !switch.bit())
        }
    }
}

/// A rotation applied to the sensor's motion, in whole degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "i8", into = "i8")]
pub struct AngleAdjustment(i8);

impl AngleAdjustment {
    /// The most the sensor can be turned either way.
    pub const LIMIT: i8 = 30;
    /// No rotation.
    pub const ZERO: Self = Self(0);

    /// A rotation of `degrees`, within ±[`Self::LIMIT`].
    ///
    /// # Errors
    ///
    /// [`OutOfRange`] beyond the limit.
    pub const fn new(degrees: i8) -> Result<Self, OutOfRange> {
        if degrees >= -Self::LIMIT && degrees <= Self::LIMIT {
            Ok(Self(degrees))
        } else {
            Err(OutOfRange {
                value: degrees as i16,
                min: -(Self::LIMIT as i16),
                max: Self::LIMIT as i16,
            })
        }
    }

    /// The rotation in degrees.
    #[must_use]
    pub const fn degrees(self) -> i8 {
        self.0
    }
}

impl TryFrom<i8> for AngleAdjustment {
    type Error = OutOfRange;

    fn try_from(degrees: i8) -> Result<Self, Self::Error> {
        Self::new(degrees)
    }
}

impl From<AngleAdjustment> for i8 {
    fn from(angle: AngleAdjustment) -> Self {
        angle.0
    }
}

/// How long the mouse stays awake without movement before it sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IdleTimeout {
    /// 10 seconds.
    Seconds10,
    /// 30 seconds.
    Seconds30,
    /// 1 minute.
    Minutes1,
    /// 2 minutes.
    Minutes2,
    /// 3 minutes.
    Minutes3,
    /// 5 minutes.
    Minutes5,
    /// 10 minutes.
    Minutes10,
    /// 15 minutes.
    Minutes15,
    /// 30 minutes.
    Minutes30,
    /// 35 minutes.
    Minutes35,
    /// 40 minutes.
    Minutes40,
}

impl IdleTimeout {
    /// Every timeout, shortest first.
    pub const ALL: [Self; 11] = [
        Self::Seconds10,
        Self::Seconds30,
        Self::Minutes1,
        Self::Minutes2,
        Self::Minutes3,
        Self::Minutes5,
        Self::Minutes10,
        Self::Minutes15,
        Self::Minutes30,
        Self::Minutes35,
        Self::Minutes40,
    ];

    /// The timeout in seconds.
    #[must_use]
    pub const fn seconds(self) -> u16 {
        match self {
            Self::Seconds10 => 10,
            Self::Seconds30 => 30,
            Self::Minutes1 => 60,
            Self::Minutes2 => 120,
            Self::Minutes3 => 180,
            Self::Minutes5 => 300,
            Self::Minutes10 => 600,
            Self::Minutes15 => 900,
            Self::Minutes30 => 1800,
            Self::Minutes35 => 2100,
            Self::Minutes40 => 2400,
        }
    }
}

/// How long a button press is held back to ignore contact bounce, in
/// milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct ButtonDebounce(u8);

impl ButtonDebounce {
    /// The longest delay offered.
    pub const MAX_MS: u8 = 30;

    /// A delay of `milliseconds`, at most [`Self::MAX_MS`].
    ///
    /// # Errors
    ///
    /// [`OutOfRange`] beyond the maximum.
    pub const fn new(milliseconds: u8) -> Result<Self, OutOfRange> {
        if milliseconds <= Self::MAX_MS {
            Ok(Self(milliseconds))
        } else {
            Err(OutOfRange {
                value: milliseconds as i16,
                min: 0,
                max: Self::MAX_MS as i16,
            })
        }
    }

    /// The delay in milliseconds.
    #[must_use]
    pub const fn milliseconds(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ButtonDebounce {
    type Error = OutOfRange;

    fn try_from(milliseconds: u8) -> Result<Self, Self::Error> {
        Self::new(milliseconds)
    }
}

impl From<ButtonDebounce> for u8 {
    fn from(debounce: ButtonDebounce) -> Self {
        debounce.0
    }
}

/// How long a wheel step is held back to ignore bounce, in milliseconds:
/// 10 to 100 in steps of 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct WheelDebounce(u8);

impl WheelDebounce {
    /// The shortest delay offered.
    pub const MIN_MS: u8 = 10;
    /// The longest delay offered.
    pub const MAX_MS: u8 = 100;
    /// Distance between two offered delays.
    pub const STEP_MS: u8 = 10;

    /// A delay of `milliseconds`: a multiple of [`Self::STEP_MS`] from
    /// [`Self::MIN_MS`] to [`Self::MAX_MS`].
    ///
    /// # Errors
    ///
    /// [`OutOfRange`] for any other value.
    pub const fn new(milliseconds: u8) -> Result<Self, OutOfRange> {
        if milliseconds >= Self::MIN_MS
            && milliseconds <= Self::MAX_MS
            && milliseconds.is_multiple_of(Self::STEP_MS)
        {
            Ok(Self(milliseconds))
        } else {
            Err(OutOfRange {
                value: milliseconds as i16,
                min: Self::MIN_MS as i16,
                max: Self::MAX_MS as i16,
            })
        }
    }

    /// The delay in milliseconds.
    #[must_use]
    pub const fn milliseconds(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for WheelDebounce {
    type Error = OutOfRange;

    fn try_from(milliseconds: u8) -> Result<Self, Self::Error> {
        Self::new(milliseconds)
    }
}

impl From<WheelDebounce> for u8 {
    fn from(debounce: WheelDebounce) -> Self {
        debounce.0
    }
}

/// A position on a ten-step slider, 1 (lowest) to 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Level(u8);

impl Level {
    /// The lowest position.
    pub const MIN: Self = Self(1);
    /// The highest position.
    pub const MAX: Self = Self(10);

    /// Position `step` on the slider.
    ///
    /// # Errors
    ///
    /// [`OutOfRange`] outside 1 to 10.
    pub const fn new(step: u8) -> Result<Self, OutOfRange> {
        if step >= Self::MIN.0 && step <= Self::MAX.0 {
            Ok(Self(step))
        } else {
            Err(OutOfRange {
                value: step as i16,
                min: Self::MIN.0 as i16,
                max: Self::MAX.0 as i16,
            })
        }
    }

    /// The position, 1 to 10.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Level {
    type Error = OutOfRange;

    fn try_from(step: u8) -> Result<Self, Self::Error> {
        Self::new(step)
    }
}

impl From<Level> for u8 {
    fn from(level: Level) -> Self {
        level.0
    }
}

/// What the LED that marks the active DPI stage does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DpiLedEffect {
    /// Shines steadily.
    Steady,
    /// Fades in and out.
    Breathing,
}

impl DpiLedEffect {
    /// Both effects.
    pub const ALL: [Self; 2] = [Self::Steady, Self::Breathing];
}

/// The LED that marks the active DPI stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DpiLed {
    /// The effect, kept while the LED is off.
    pub effect: DpiLedEffect,
    /// How bright it shines.
    pub brightness: Level,
    /// How fast a [`DpiLedEffect::Breathing`] LED cycles.
    pub speed: Level,
    /// Whether it lights at all.
    pub enabled: bool,
}

/// The decorative effect of the mouse's body light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BodyLightMode {
    /// No light.
    Off,
    /// Cycling rainbow.
    Rainbow,
    /// One colour fading in and out.
    ColorBreath,
    /// One steady colour.
    FixedColor,
    /// Neon sweep.
    Neon,
    /// Rainbow fading in and out.
    RainbowBreath,
    /// Steady rainbow.
    FixedRainbow,
}

impl BodyLightMode {
    /// The effects that light the mouse, in display order.
    pub const LIT: [Self; 6] = [
        Self::Rainbow,
        Self::ColorBreath,
        Self::FixedColor,
        Self::Neon,
        Self::RainbowBreath,
        Self::FixedRainbow,
    ];

    /// Whether the effect shows [`BodyLight::color`].
    #[must_use]
    pub const fn uses_color(self) -> bool {
        matches!(self, Self::ColorBreath | Self::FixedColor)
    }
}

/// The mouse's decorative body light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BodyLight {
    /// The effect, kept while the light is off.
    pub mode: BodyLightMode,
    /// The colour the colour effects use.
    pub color: Rgb,
    /// How fast the effect runs.
    pub speed: Level,
    /// How bright it shines.
    pub brightness: Level,
    /// Whether it lights at all.
    pub enabled: bool,
    /// Whether it goes dark while the mouse moves.
    pub off_while_moving: bool,
}

impl BodyLight {
    /// Whether the light shines now: switched on with an effect chosen.
    #[must_use]
    pub fn is_lit(&self) -> bool {
        self.enabled && self.mode != BodyLightMode::Off
    }

    /// This light running `mode`, or switched off for [`BodyLightMode::Off`].
    /// Switching off keeps the remembered effect and clears the
    /// off-while-moving option, as the vendor app does.
    #[must_use]
    pub fn selecting(self, mode: BodyLightMode) -> Self {
        if mode == BodyLightMode::Off {
            Self {
                enabled: false,
                off_while_moving: false,
                ..self
            }
        } else {
            Self {
                mode,
                enabled: true,
                ..self
            }
        }
    }
}

/// Everything a mouse stores besides its DPI stages and polling rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    /// Lift-off distance.
    pub lift_off: LiftOffDistance,
    /// Sensor power profile.
    pub sensor_mode: SensorMode,
    /// The on/off sensor options.
    pub switches: SensorSwitches,
    /// Rotation of the sensor's motion.
    pub angle_adjustment: AngleAdjustment,
    /// Time without movement before the mouse sleeps.
    pub idle_timeout: IdleTimeout,
    /// Button debounce.
    pub button_debounce: ButtonDebounce,
    /// Wheel debounce, or `None` while the mouse has none stored.
    pub wheel_debounce: Option<WheelDebounce>,
    /// The DPI indicator LED.
    pub dpi_led: DpiLed,
    /// The decorative body light.
    pub body_light: BodyLight,
}

/// One edit of [`MouseSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseSettingsChange {
    /// Set the lift-off distance.
    LiftOff(LiftOffDistance),
    /// Set the sensor power profile.
    SensorMode(SensorMode),
    /// Turn one sensor option on or off.
    Switch {
        /// The option.
        switch: SensorSwitch,
        /// Its new state.
        on: bool,
    },
    /// Rotate the sensor's motion.
    AngleAdjustment(AngleAdjustment),
    /// Set the sleep timeout.
    IdleTimeout(IdleTimeout),
    /// Set the button debounce.
    ButtonDebounce(ButtonDebounce),
    /// Set the wheel debounce.
    WheelDebounce(WheelDebounce),
    /// Replace the DPI indicator LED.
    DpiLed(DpiLed),
    /// Replace the body light.
    BodyLight(BodyLight),
}

impl MouseSettings {
    /// These settings with `change` applied.
    #[must_use]
    pub fn applying(self, change: &MouseSettingsChange) -> Self {
        match *change {
            MouseSettingsChange::LiftOff(lift_off) => Self { lift_off, ..self },
            MouseSettingsChange::SensorMode(sensor_mode) => Self {
                sensor_mode,
                ..self
            },
            MouseSettingsChange::Switch { switch, on } => Self {
                switches: self.switches.with(switch, on),
                ..self
            },
            MouseSettingsChange::AngleAdjustment(angle_adjustment) => Self {
                angle_adjustment,
                ..self
            },
            MouseSettingsChange::IdleTimeout(idle_timeout) => Self {
                idle_timeout,
                ..self
            },
            MouseSettingsChange::ButtonDebounce(button_debounce) => Self {
                button_debounce,
                ..self
            },
            MouseSettingsChange::WheelDebounce(wheel) => Self {
                wheel_debounce: Some(wheel),
                ..self
            },
            MouseSettingsChange::DpiLed(dpi_led) => Self { dpi_led, ..self },
            MouseSettingsChange::BodyLight(body_light) => Self { body_light, ..self },
        }
    }

    /// Whether `change` is already in effect.
    #[must_use]
    pub fn includes(&self, change: &MouseSettingsChange) -> bool {
        self.applying(change) == *self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AngleAdjustment, BodyLight, BodyLightMode, ButtonDebounce, Level, SensorSwitch,
        SensorSwitches, WheelDebounce,
    };
    use crate::color::Rgb;

    fn light() -> BodyLight {
        BodyLight {
            mode: BodyLightMode::Rainbow,
            color: Rgb::WHITE,
            speed: Level::MIN,
            brightness: Level::MAX,
            enabled: true,
            off_while_moving: true,
        }
    }

    #[test]
    fn numbers_outside_their_range_are_refused() {
        assert!(AngleAdjustment::new(30).ok().is_some());
        assert!(AngleAdjustment::new(-30).ok().is_some());
        assert!(AngleAdjustment::new(31).ok().is_none());
        assert!(AngleAdjustment::new(-31).ok().is_none());
        assert!(ButtonDebounce::new(30).ok().is_some());
        assert!(ButtonDebounce::new(31).ok().is_none());
        assert!(WheelDebounce::new(10).ok().is_some());
        assert!(WheelDebounce::new(100).ok().is_some());
        assert!(WheelDebounce::new(0).ok().is_none());
        assert!(WheelDebounce::new(15).ok().is_none());
        assert!(WheelDebounce::new(110).ok().is_none());
        assert!(Level::new(0).ok().is_none());
        assert!(Level::new(11).ok().is_none());
    }

    #[test]
    fn switches_are_independent() {
        let on = SensorSwitches::default()
            .with(SensorSwitch::MotionSync, true)
            .with(SensorSwitch::TwentyKFps, true);
        assert!(on.is_on(SensorSwitch::MotionSync));
        assert!(on.is_on(SensorSwitch::TwentyKFps));
        assert!(!on.is_on(SensorSwitch::RippleControl));
        assert!(
            !on.with(SensorSwitch::MotionSync, false)
                .is_on(SensorSwitch::MotionSync)
        );
    }

    #[test]
    fn switching_the_body_light_off_keeps_its_effect() {
        let off = light().selecting(BodyLightMode::Off);
        assert!(!off.is_lit());
        assert_eq!(off.mode, BodyLightMode::Rainbow);
        assert!(!off.off_while_moving);

        let neon = off.selecting(BodyLightMode::Neon);
        assert!(neon.is_lit());
        assert_eq!(neon.mode, BodyLightMode::Neon);
    }
}
