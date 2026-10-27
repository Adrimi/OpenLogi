//! The sensor, timing and lighting settings of the configuration flash.
//!
//! Addresses and encodings are documented in `docs/COMPX_PROTOCOL.md` (the
//! map and the sections it links). Reading is lenient where the rest of the
//! image is strict: a field whose check byte fails, as erased flash does, or
//! that holds a value the vendor app never writes, reads as the vendor
//! driver's default, so an unwritten setting never makes the DPI stages and
//! polling rate unreadable.

use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    AngleAdjustment, BodyLight, BodyLightMode, ButtonDebounce, DpiLed, DpiLedEffect, IdleTimeout,
    Level, LiftOffDistance, MouseSettings, MouseSettingsChange, SensorMode, SensorSwitch,
    SensorSwitches, WheelDebounce,
};

use super::{ConfigChangeError, FLASH_LEN, field, single, with_check, write_single};
use crate::compx::Request;

const LIFT_OFF: u16 = 0x0a;
const DPI_LED_EFFECT: u16 = 0x4c;
const DPI_LED_BRIGHTNESS: u16 = 0x4e;
const DPI_LED_SPEED: u16 = 0x50;
const DPI_LED_ENABLED: u16 = 0x52;
const BODY_LIGHT: u16 = 0xa0;
const BODY_LIGHT_ENABLED: u16 = 0xa7;
const BUTTON_DEBOUNCE: u16 = 0xa9;
const IDLE_TIMEOUT: u16 = 0xad;
const BODY_LIGHT_OFF_WHILE_MOVING: u16 = 0xb3;
const IDLE_TIMEOUT_MIRROR: u16 = 0xb7;
const SENSOR_MODE: u16 = 0xb9;
const ANGLE_ADJUSTMENT: u16 = 0xbd;
const ANGLE_ADJUSTMENT_MARKER: u16 = 0xbf;
const WHEEL_DEBOUNCE: u16 = 0xe3;

/// The DPI LED brightness code of each slider position 1 to 10.
const BRIGHTNESS_CODES: [u8; 10] = [16, 30, 60, 90, 128, 150, 180, 210, 230, 255];
/// The brightness code of an unwritten DPI LED (the vendor default).
const DEFAULT_BRIGHTNESS_CODE: u8 = 0x80;

const DEFAULT_LIFT_OFF: LiftOffDistance = LiftOffDistance::Mm1;
const DEFAULT_SENSOR_MODE: SensorMode = SensorMode::LowPower;
const DEFAULT_IDLE_TIMEOUT: IdleTimeout = IdleTimeout::Minutes1;
const DEFAULT_BUTTON_DEBOUNCE_MS: u8 = 8;
const DEFAULT_DPI_LED_SPEED: u8 = 3;
/// The vendor defaults, as the stored value (slider position minus one).
const DEFAULT_BODY_LIGHT_SPEED: u8 = 8;
const DEFAULT_BODY_LIGHT_BRIGHTNESS: u8 = 9;

fn level(step: u8) -> Level {
    Level::new(step.clamp(Level::MIN.get(), Level::MAX.get())).unwrap_or(Level::MIN)
}

fn lenient<T>(
    bytes: &[u8; FLASH_LEN],
    address: u16,
    decode: impl FnOnce(u8) -> Option<T>,
) -> Option<T> {
    single(bytes, address).ok().and_then(decode)
}

fn switch_address(switch: SensorSwitch) -> u16 {
    match switch {
        SensorSwitch::MotionSync => 0xab,
        SensorSwitch::AngleSnapping => 0xaf,
        SensorSwitch::RippleControl => 0xb1,
        SensorSwitch::HighestPerformance => 0xb5,
        SensorSwitch::TwentyKFps => 0xe1,
    }
}

fn default_switch(switch: SensorSwitch) -> bool {
    switch == SensorSwitch::MotionSync
}

fn lift_off_code(distance: LiftOffDistance) -> u8 {
    match distance {
        LiftOffDistance::Mm0Point7 => 3,
        LiftOffDistance::Mm1 => 1,
        LiftOffDistance::Mm2 => 2,
    }
}

fn sensor_mode_code(mode: SensorMode) -> u8 {
    match mode {
        SensorMode::LowPower => 0,
        SensorMode::HighPerformance => 1,
    }
}

/// The idle timeout in units of ten seconds; the longest, 40 minutes, is 240.
fn idle_timeout_code(timeout: IdleTimeout) -> u8 {
    u8::try_from(timeout.seconds() / 10).unwrap_or(u8::MAX)
}

fn dpi_led_effect_code(effect: DpiLedEffect) -> u8 {
    match effect {
        DpiLedEffect::Steady => 1,
        DpiLedEffect::Breathing => 2,
    }
}

fn brightness_code(level: Level) -> u8 {
    BRIGHTNESS_CODES[usize::from(level.get() - 1)]
}

fn brightness_level(code: u8) -> Level {
    let nearest = (Level::MIN.get()..=Level::MAX.get())
        .zip(BRIGHTNESS_CODES)
        .min_by_key(|(_, candidate)| candidate.abs_diff(code))
        .map_or(Level::MIN.get(), |(step, _)| step);
    level(nearest)
}

fn body_light_mode_code(mode: BodyLightMode) -> u8 {
    match mode {
        BodyLightMode::Off => 0,
        BodyLightMode::Rainbow => 1,
        BodyLightMode::ColorBreath => 2,
        BodyLightMode::FixedColor => 3,
        BodyLightMode::Neon => 4,
        BodyLightMode::RainbowBreath => 5,
        BodyLightMode::FixedRainbow => 6,
    }
}

fn dpi_led(bytes: &[u8; FLASH_LEN]) -> DpiLed {
    DpiLed {
        effect: lenient(bytes, DPI_LED_EFFECT, |code| {
            DpiLedEffect::ALL
                .into_iter()
                .find(|effect| dpi_led_effect_code(*effect) == code)
        })
        .unwrap_or(DpiLedEffect::Breathing),
        brightness: brightness_level(
            lenient(bytes, DPI_LED_BRIGHTNESS, Some).unwrap_or(DEFAULT_BRIGHTNESS_CODE),
        ),
        speed: level(lenient(bytes, DPI_LED_SPEED, Some).unwrap_or(DEFAULT_DPI_LED_SPEED)),
        enabled: lenient(bytes, DPI_LED_ENABLED, |code| Some(code != 0)).unwrap_or(true),
    }
}

fn body_light(bytes: &[u8; FLASH_LEN]) -> BodyLight {
    let record = field(bytes, BODY_LIGHT, 6).ok();
    let stored = |index: usize, default: u8| record.map_or(default, |record| record[index]);
    BodyLight {
        mode: [BodyLightMode::Off]
            .into_iter()
            .chain(BodyLightMode::LIT)
            .find(|mode| body_light_mode_code(*mode) == stored(0, 1))
            .unwrap_or(BodyLightMode::Rainbow),
        color: Rgb::new(stored(1, 255), stored(2, 255), stored(3, 255)),
        speed: level(stored(4, DEFAULT_BODY_LIGHT_SPEED).saturating_add(1)),
        brightness: level(stored(5, DEFAULT_BODY_LIGHT_BRIGHTNESS).saturating_add(1)),
        enabled: lenient(bytes, BODY_LIGHT_ENABLED, |code| Some(code != 0)).unwrap_or(true),
        off_while_moving: lenient(bytes, BODY_LIGHT_OFF_WHILE_MOVING, |code| Some(code != 0))
            .unwrap_or(false),
    }
}

/// Decode the settings of a complete image.
pub(super) fn parse(bytes: &[u8; FLASH_LEN]) -> MouseSettings {
    let switches =
        SensorSwitch::ALL
            .into_iter()
            .fold(SensorSwitches::default(), |switches, switch| {
                let on = lenient(bytes, switch_address(switch), |code| Some(code != 0))
                    .unwrap_or_else(|| default_switch(switch));
                switches.with(switch, on)
            });
    MouseSettings {
        lift_off: lenient(bytes, LIFT_OFF, |code| {
            LiftOffDistance::ALL
                .into_iter()
                .find(|distance| lift_off_code(*distance) == code)
        })
        .unwrap_or(DEFAULT_LIFT_OFF),
        sensor_mode: lenient(bytes, SENSOR_MODE, |code| {
            SensorMode::ALL
                .into_iter()
                .find(|mode| sensor_mode_code(*mode) == code)
        })
        .unwrap_or(DEFAULT_SENSOR_MODE),
        switches,
        angle_adjustment: lenient(bytes, ANGLE_ADJUSTMENT, |code| {
            AngleAdjustment::new(code.cast_signed()).ok()
        })
        .unwrap_or(AngleAdjustment::ZERO),
        idle_timeout: lenient(bytes, IDLE_TIMEOUT, |code| {
            IdleTimeout::ALL
                .into_iter()
                .find(|timeout| idle_timeout_code(*timeout) == code)
        })
        .unwrap_or(DEFAULT_IDLE_TIMEOUT),
        button_debounce: lenient(bytes, BUTTON_DEBOUNCE, |ms| ButtonDebounce::new(ms).ok())
            .or_else(|| ButtonDebounce::new(DEFAULT_BUTTON_DEBOUNCE_MS).ok())
            .unwrap_or_else(|| unreachable!("the default debounce is in range")),
        wheel_debounce: lenient(bytes, WHEEL_DEBOUNCE, |ms| WheelDebounce::new(ms).ok()),
        dpi_led: dpi_led(bytes),
        body_light: body_light(bytes),
    }
}

/// Flash writes applying `change`.
///
/// Every value in a [`MouseSettingsChange`] is in range by construction, so
/// this fails only if a framed request cannot be built.
///
/// # Errors
///
/// [`ConfigChangeError::Request`] when a request cannot be framed.
pub fn change_requests(change: &MouseSettingsChange) -> Result<Vec<Request>, ConfigChangeError> {
    Ok(match *change {
        MouseSettingsChange::LiftOff(distance) => {
            vec![write_single(LIFT_OFF, lift_off_code(distance))]
        }
        MouseSettingsChange::SensorMode(mode) => {
            vec![write_single(SENSOR_MODE, sensor_mode_code(mode))]
        }
        MouseSettingsChange::Switch { switch, on } => {
            vec![write_single(switch_address(switch), u8::from(on))]
        }
        MouseSettingsChange::AngleAdjustment(angle) => vec![
            write_single(ANGLE_ADJUSTMENT, angle.degrees().cast_unsigned()),
            write_single(ANGLE_ADJUSTMENT_MARKER, 1),
        ],
        MouseSettingsChange::IdleTimeout(timeout) => {
            let code = idle_timeout_code(timeout);
            vec![
                write_single(IDLE_TIMEOUT, code),
                write_single(IDLE_TIMEOUT_MIRROR, code),
            ]
        }
        MouseSettingsChange::ButtonDebounce(debounce) => {
            vec![write_single(BUTTON_DEBOUNCE, debounce.milliseconds())]
        }
        MouseSettingsChange::WheelDebounce(debounce) => {
            vec![write_single(WHEEL_DEBOUNCE, debounce.milliseconds())]
        }
        MouseSettingsChange::DpiLed(led) => vec![
            write_single(DPI_LED_EFFECT, dpi_led_effect_code(led.effect)),
            write_single(DPI_LED_BRIGHTNESS, brightness_code(led.brightness)),
            write_single(DPI_LED_SPEED, led.speed.get()),
            write_single(DPI_LED_ENABLED, u8::from(led.enabled)),
        ],
        MouseSettingsChange::BodyLight(light) => {
            let (r, g, b) = light.color.components();
            let record: [u8; 7] = with_check([
                body_light_mode_code(light.mode),
                r,
                g,
                b,
                light.speed.get() - 1,
                light.brightness.get() - 1,
            ]);
            vec![
                Request::write_flash(BODY_LIGHT, &record)?,
                write_single(BODY_LIGHT_ENABLED, u8::from(light.enabled)),
                write_single(
                    BODY_LIGHT_OFF_WHILE_MOVING,
                    u8::from(light.off_while_moving),
                ),
            ]
        }
    })
}
