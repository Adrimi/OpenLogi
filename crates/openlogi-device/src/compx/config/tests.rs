use super::{
    ButtonAction, ConfigChangeError, ConfigError, Dpi, DpiAction, DpiStage, FLASH_LEN, FlashImage,
    MouseButton, MouseConfig, ReportRate, Rgb, Scroll, change_requests, set_active_stage,
    set_button, set_report_rate, set_stages,
};
use openlogi_core::color::Rgb as WireRgb;
use openlogi_core::hid::{
    AngleAdjustment, BodyLight, BodyLightMode, ButtonDebounce, DpiLed, DpiLedEffect, IdleTimeout,
    Level, LiftOffDistance, MouseSettingsChange, SensorMode, SensorSwitch, WheelDebounce,
};

use crate::compx::{PAYLOAD_LEN, REPORT_LEN, Request, Response, checksum};

// The configuration flash of a Zaopin mouse behind a 3554:f524 receiver.
const DUMP: &str = "
    01 54 05 50 03 52 00 55 00 55 01 54 0f 0f 00 37
    13 13 00 2f 17 17 00 27 1b 1b 00 1f 1f 1f 00 17
    7f 7f 00 57 7f 7f 00 57 7f 7f 00 57 ff 00 00 56
    00 ff 00 56 ff ff 00 57 00 00 ff 56 ff 80 00 d6
    00 ff ff 57 00 ff ff 57 ff ff ff 58 02 53 ff 56
    03 52 00 55 ff 00 ff 57 00 55 80 d5 03 52 00 55
    01 01 00 53 01 02 00 52 01 04 00 50 01 08 00 4c
    01 10 00 44 02 01 00 52 02 02 00 51 02 03 00 50
    07 00 00 4e 08 04 00 49 04 0a 03 44 00 00 00 55
    00 00 00 55 00 00 00 55 00 00 00 55 00 00 00 55
    01 ff 00 ff 07 09 46 00 55 08 4d 01 54 06 4f 00
    55 00 55 00 55 00 55 06 4f 01 54 0a 4b ff ff ff
    ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff
    ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff
    ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff ff";

pub(in crate::compx) fn dump() -> Vec<u8> {
    let bytes: Vec<u8> = DUMP
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("hex byte"))
        .collect();
    assert_eq!(bytes.len(), FLASH_LEN);
    bytes
}

fn reply_for(request: &Request, flash: &[u8]) -> Response {
    let bytes = request.as_bytes();
    let address = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
    let length = usize::from(bytes[5]);
    let mut frame = [0u8; REPORT_LEN];
    frame[..6].copy_from_slice(&bytes[..6]);
    frame[6..6 + length].copy_from_slice(&flash[address..address + length]);
    frame[16] = checksum(&frame[..16]);
    Response::parse(&frame).expect("valid reply")
}

fn image_of(flash: &[u8]) -> FlashImage {
    let mut image = FlashImage::default();
    for request in FlashImage::read_requests() {
        image
            .accept(&reply_for(&request, flash))
            .expect("chunk accepted");
    }
    image
}

fn color(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

#[test]
fn read_requests_cover_the_image_in_ten_byte_chunks() {
    let requests: Vec<_> = FlashImage::read_requests().collect();

    assert_eq!(requests.len(), FLASH_LEN.div_ceil(PAYLOAD_LEN));
    assert_eq!(&requests[0].as_bytes()[1..6], &[0x08, 0, 0, 0, 10]);
    assert_eq!(&requests[23].as_bytes()[1..6], &[0x08, 0, 0, 0xe6, 10]);
}

#[test]
fn hardware_dump_decodes_to_the_mouse_settings() {
    let image = image_of(&dump());
    assert_eq!(image.as_bytes().as_slice(), dump().as_slice());

    let config = MouseConfig::parse(&image).expect("valid configuration");
    let stage = |dpi, color| DpiStage {
        x: Dpi::new(dpi).expect("valid dpi"),
        y: Dpi::new(dpi).expect("valid dpi"),
        color,
    };

    assert_eq!(config.report_rate, ReportRate::Hz1000);
    assert_eq!(
        config.stages,
        vec![
            stage(800, color(0xff, 0, 0)),
            stage(1000, color(0, 0xff, 0)),
            stage(1200, color(0xff, 0xff, 0)),
            stage(1400, color(0, 0, 0xff)),
            stage(1600, color(0xff, 0x80, 0)),
        ]
    );
    assert_eq!(config.active_stage, 3);
    assert_eq!(config.current_stage().x.get(), 1400);
    assert_eq!(config.settings.button_debounce.milliseconds(), 8);
    assert_eq!(
        config.buttons[..11],
        [
            ButtonAction::Mouse(MouseButton::Left),
            ButtonAction::Mouse(MouseButton::Right),
            ButtonAction::Mouse(MouseButton::Middle),
            ButtonAction::Mouse(MouseButton::Back),
            ButtonAction::Mouse(MouseButton::Forward),
            ButtonAction::Dpi(DpiAction::Cycle),
            ButtonAction::Dpi(DpiAction::Up),
            ButtonAction::Dpi(DpiAction::Down),
            ButtonAction::ReportRateCycle,
            ButtonAction::Unrecognized {
                kind: 0x08,
                parameters: [0x04, 0x00],
            },
            ButtonAction::Fire([0x0a, 0x03]),
        ]
    );
    assert!(
        config.buttons[11..]
            .iter()
            .all(|button| *button == ButtonAction::Disabled)
    );
}

#[test]
fn a_missing_chunk_is_reported() {
    let flash = dump();
    let mut image = FlashImage::default();
    for request in FlashImage::read_requests().skip(1) {
        image
            .accept(&reply_for(&request, &flash))
            .expect("accepted");
    }

    assert_eq!(MouseConfig::parse(&image), Err(ConfigError::Incomplete(0)));
}

#[test]
fn a_reply_outside_the_image_is_refused() {
    let request = Request::read_flash(0x0005, 10).expect("misaligned read");
    let mut image = FlashImage::default();

    assert_eq!(
        image.accept(&reply_for(&request, &dump())),
        Err(ConfigError::UnexpectedReply)
    );
}

#[test]
fn a_failed_check_byte_is_corruption() {
    let mut flash = dump();
    flash[0x0d] = 0x10;

    assert_eq!(
        MouseConfig::parse(&image_of(&flash)),
        Err(ConfigError::Corrupt(0x0c))
    );
}

#[test]
fn unknown_rates_and_dpi_high_bits_are_unsupported() {
    let mut flash = dump();
    flash[0] = 0x03;
    flash[1] = 0x52;
    assert_eq!(
        MouseConfig::parse(&image_of(&flash)),
        Err(ConfigError::Unsupported {
            address: 0x00,
            value: 0x03,
        })
    );

    let mut flash = dump();
    flash[0x0e] = 0x01;
    flash[0x0f] = 0x36;
    assert_eq!(
        MouseConfig::parse(&image_of(&flash)),
        Err(ConfigError::Unsupported {
            address: 0x0c,
            value: 0x01,
        })
    );
}

#[test]
fn an_active_stage_past_the_list_is_unsupported() {
    let mut flash = dump();
    flash[4] = 0x05;
    flash[5] = 0x50;

    assert_eq!(
        MouseConfig::parse(&image_of(&flash)),
        Err(ConfigError::Unsupported {
            address: 0x04,
            value: 0x05,
        })
    );
}

#[test]
fn dpi_accepts_only_storable_values() {
    assert!(Dpi::new(50).is_some());
    assert!(Dpi::new(12_800).is_some());
    assert!(Dpi::new(0).is_none());
    assert!(Dpi::new(825).is_none());
    assert!(Dpi::new(12_850).is_none());
}

fn apply(flash: &mut [u8], request: &Request) {
    let bytes = request.as_bytes();
    assert_eq!(bytes[1], 0x07, "flash write");
    let address = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
    let length = usize::from(bytes[5]);
    flash[address..address + length].copy_from_slice(&bytes[6..6 + length]);
}

#[test]
fn scalar_writes_round_trip_through_the_parser() {
    let mut flash = dump();
    apply(&mut flash, &set_report_rate(ReportRate::Hz4000));
    for request in change_requests(&MouseSettingsChange::ButtonDebounce(
        ButtonDebounce::new(4).expect("in range"),
    ))
    .expect("framed")
    {
        apply(&mut flash, &request);
    }
    let config = MouseConfig::parse(&image_of(&flash)).expect("valid");
    apply(
        &mut flash,
        &set_active_stage(&config, 0).expect("stage in range"),
    );

    let config = MouseConfig::parse(&image_of(&flash)).expect("valid");
    assert_eq!(config.report_rate, ReportRate::Hz4000);
    assert_eq!(config.settings.button_debounce.milliseconds(), 4);
    assert_eq!(config.active_stage, 0);
}

#[test]
fn stage_writes_round_trip_through_the_parser() {
    let stages = [
        DpiStage {
            x: Dpi::new(400).expect("dpi"),
            y: Dpi::new(400).expect("dpi"),
            color: color(1, 2, 3),
        },
        DpiStage {
            x: Dpi::new(3200).expect("dpi"),
            y: Dpi::new(1600).expect("dpi"),
            color: color(4, 5, 6),
        },
    ];
    let mut flash = dump();
    for request in set_stages(&stages, 1).expect("valid stages") {
        apply(&mut flash, &request);
    }

    let config = MouseConfig::parse(&image_of(&flash)).expect("valid");
    assert_eq!(config.stages, stages);
    assert_eq!(config.active_stage, 1);
}

#[test]
fn button_writes_round_trip_through_the_parser() {
    let mut flash = dump();
    apply(
        &mut flash,
        &set_button(15, ButtonAction::Scroll(Scroll::Down)).expect("in range"),
    );
    apply(
        &mut flash,
        &set_button(14, ButtonAction::Scroll(Scroll::Left)).expect("in range"),
    );

    let config = MouseConfig::parse(&image_of(&flash)).expect("valid");
    assert_eq!(config.buttons[15], ButtonAction::Scroll(Scroll::Down));
    assert_eq!(config.buttons[14], ButtonAction::Scroll(Scroll::Left));
}

#[test]
fn invalid_changes_are_refused_before_framing() {
    let config = MouseConfig::parse(&image_of(&dump())).expect("valid");

    assert_eq!(
        set_active_stage(&config, 5),
        Err(ConfigChangeError::StageOutOfRange { stage: 5, count: 5 })
    );
    assert_eq!(set_stages(&[], 0), Err(ConfigChangeError::StageCount(0)));
    assert_eq!(
        set_button(16, ButtonAction::Disabled),
        Err(ConfigChangeError::ButtonOutOfRange(16))
    );
}

fn level(step: u8) -> Level {
    Level::new(step).expect("level")
}

fn after(flash: &[u8], change: MouseSettingsChange) -> (Vec<u8>, MouseConfig) {
    let mut flash = flash.to_vec();
    for request in change_requests(&change).expect("framed") {
        apply(&mut flash, &request);
    }
    let config = MouseConfig::parse(&image_of(&flash)).expect("valid");
    (flash, config)
}

#[test]
fn hardware_dump_decodes_to_the_sensor_timing_and_lighting_settings() {
    let settings = MouseConfig::parse(&image_of(&dump()))
        .expect("valid configuration")
        .settings;

    assert_eq!(settings.lift_off, LiftOffDistance::Mm1);
    assert_eq!(settings.sensor_mode, SensorMode::HighPerformance);
    assert!(settings.switches.is_on(SensorSwitch::MotionSync));
    for off in [
        SensorSwitch::AngleSnapping,
        SensorSwitch::RippleControl,
        SensorSwitch::HighestPerformance,
        SensorSwitch::TwentyKFps,
    ] {
        assert!(!settings.switches.is_on(off), "{off:?}");
    }
    assert_eq!(settings.angle_adjustment, AngleAdjustment::ZERO);
    assert_eq!(settings.idle_timeout, IdleTimeout::Minutes1);
    assert_eq!(settings.button_debounce.milliseconds(), 8);
    assert_eq!(settings.wheel_debounce, None);
    assert_eq!(
        settings.dpi_led,
        DpiLed {
            effect: DpiLedEffect::Breathing,
            brightness: level(10),
            speed: level(3),
            enabled: false,
        }
    );
    assert_eq!(
        settings.body_light,
        BodyLight {
            mode: BodyLightMode::Rainbow,
            color: WireRgb::new(0xff, 0, 0xff),
            speed: level(8),
            brightness: level(10),
            enabled: false,
            off_while_moving: false,
        }
    );
}

#[test]
fn every_setting_change_round_trips_through_the_parser() {
    let flash = dump();
    let changes = [
        MouseSettingsChange::LiftOff(LiftOffDistance::Mm0Point7),
        MouseSettingsChange::LiftOff(LiftOffDistance::Mm2),
        MouseSettingsChange::SensorMode(SensorMode::LowPower),
        MouseSettingsChange::Switch {
            switch: SensorSwitch::MotionSync,
            on: false,
        },
        MouseSettingsChange::Switch {
            switch: SensorSwitch::AngleSnapping,
            on: true,
        },
        MouseSettingsChange::Switch {
            switch: SensorSwitch::RippleControl,
            on: true,
        },
        MouseSettingsChange::Switch {
            switch: SensorSwitch::HighestPerformance,
            on: true,
        },
        MouseSettingsChange::Switch {
            switch: SensorSwitch::TwentyKFps,
            on: true,
        },
        MouseSettingsChange::AngleAdjustment(AngleAdjustment::new(-30).expect("in range")),
        MouseSettingsChange::AngleAdjustment(AngleAdjustment::new(17).expect("in range")),
        MouseSettingsChange::ButtonDebounce(ButtonDebounce::new(0).expect("in range")),
        MouseSettingsChange::WheelDebounce(WheelDebounce::new(60).expect("in range")),
        MouseSettingsChange::DpiLed(DpiLed {
            effect: DpiLedEffect::Steady,
            brightness: level(1),
            speed: level(10),
            enabled: true,
        }),
        MouseSettingsChange::BodyLight(BodyLight {
            mode: BodyLightMode::FixedColor,
            color: WireRgb::new(1, 2, 3),
            speed: level(1),
            brightness: level(5),
            enabled: true,
            off_while_moving: true,
        }),
    ];
    for change in changes {
        let (_, config) = after(&flash, change);
        let expected = MouseConfig::parse(&image_of(&flash))
            .expect("valid")
            .settings
            .applying(&change);
        assert_eq!(config.settings, expected, "{change:?}");
        assert!(config.settings.includes(&change), "{change:?}");
    }
}

#[test]
fn idle_timeout_writes_both_copies_for_every_allowed_code() {
    let codes = [1u8, 3, 6, 12, 18, 30, 60, 90, 180, 210, 240];
    for (timeout, code) in IdleTimeout::ALL.into_iter().zip(codes) {
        let (flash, config) = after(&dump(), MouseSettingsChange::IdleTimeout(timeout));
        assert_eq!(config.settings.idle_timeout, timeout);
        let pair = [code, 0x55u8.wrapping_sub(code)];
        assert_eq!(&flash[0xad..0xaf], &pair);
        assert_eq!(&flash[0xb7..0xb9], &pair);
    }
}

#[test]
fn angle_adjustment_is_signed_and_marked() {
    let angle = AngleAdjustment::new(-5).expect("in range");
    let (flash, config) = after(&dump(), MouseSettingsChange::AngleAdjustment(angle));

    assert_eq!(&flash[0xbd..0xbf], &[0xfb, 0x5a]);
    assert_eq!(&flash[0xbf..0xc1], &[0x01, 0x54]);
    assert_eq!(config.settings.angle_adjustment, angle);
}

#[test]
fn body_light_and_dpi_led_use_the_vendor_encodings() {
    let light = BodyLight {
        mode: BodyLightMode::Neon,
        color: WireRgb::new(0x10, 0x20, 0x30),
        speed: level(3),
        brightness: level(6),
        enabled: true,
        off_while_moving: true,
    };
    let (flash, _) = after(&dump(), MouseSettingsChange::BodyLight(light));
    assert_eq!(&flash[0xa0..0xa6], &[0x04, 0x10, 0x20, 0x30, 0x02, 0x05]);
    assert_eq!(flash[0xa6], checksum(&flash[0xa0..0xa6]));
    assert_eq!(&flash[0xa7..0xa9], &[0x01, 0x54]);
    assert_eq!(&flash[0xb3..0xb5], &[0x01, 0x54]);

    let led = DpiLed {
        effect: DpiLedEffect::Steady,
        brightness: level(5),
        speed: level(7),
        enabled: true,
    };
    let (flash, _) = after(&dump(), MouseSettingsChange::DpiLed(led));
    assert_eq!(&flash[0x4c..0x4e], &[0x01, 0x54]);
    assert_eq!(&flash[0x4e..0x50], &[0x80, 0xd5]);
    assert_eq!(&flash[0x50..0x52], &[0x07, 0x4e]);
    assert_eq!(&flash[0x52..0x54], &[0x01, 0x54]);
}

#[test]
fn unwritten_or_unknown_settings_read_as_the_vendor_defaults() {
    let mut flash = dump();
    // Erase the lift-off distance and sensor mode, and store a lift-off code
    // the vendor app never writes.
    flash[0x0a] = 0xff;
    flash[0x0b] = 0xff;
    flash[0xb9] = 0x07;
    flash[0xba] = 0x55 - 0x07;
    let settings = MouseConfig::parse(&image_of(&flash))
        .expect("an unwritten setting does not fail the read")
        .settings;

    assert_eq!(settings.lift_off, LiftOffDistance::Mm1);
    assert_eq!(settings.sensor_mode, SensorMode::LowPower);
}
