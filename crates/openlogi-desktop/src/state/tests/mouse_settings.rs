//! Mouse settings: a mouse that stores its own settings shows and edits them,
//! writes one change at a time, and trusts only the newest read-back.

use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    AngleAdjustment, BodyLight, BodyLightMode, ButtonDebounce, DpiLed, DpiLedEffect, IdleTimeout,
    Level, LiftOffDistance, MouseSettings, MouseSettingsChange, SensorMode, SensorSwitch,
    SensorSwitches, WheelDebounce,
};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadMouseSettings, SetMouseSettings};

type Commands = tokio::sync::mpsc::UnboundedReceiver<Command>;

fn settings() -> MouseSettings {
    MouseSettings {
        lift_off: LiftOffDistance::Mm1,
        sensor_mode: SensorMode::HighPerformance,
        switches: SensorSwitches::default().with(SensorSwitch::MotionSync, true),
        angle_adjustment: AngleAdjustment::ZERO,
        idle_timeout: IdleTimeout::Minutes1,
        button_debounce: ButtonDebounce::new(8).expect("in range"),
        wheel_debounce: Some(WheelDebounce::new(20).expect("in range")),
        dpi_led: DpiLed {
            effect: DpiLedEffect::Breathing,
            brightness: Level::MAX,
            speed: Level::new(3).expect("level"),
            enabled: false,
        },
        body_light: BodyLight {
            mode: BodyLightMode::Rainbow,
            color: Rgb::WHITE,
            speed: Level::new(8).expect("level"),
            brightness: Level::MAX,
            enabled: false,
            off_while_moving: false,
        },
    }
}

fn mouse_state(supported: bool) -> (AppState, DeviceKey, Commands) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .mouse_settings = supported;
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let key = state.current_record().expect("selected").device_key();
    (state, key, receiver)
}

fn loaded_state() -> (AppState, DeviceKey, Commands) {
    let (mut state, key, mut receiver) = mouse_state(true);
    state.load_current_mouse_settings();
    let _ = state.apply_mouse_settings_read(&key, Ok(settings()));
    drain(&mut receiver);
    (state, key, receiver)
}

fn writes(receiver: &mut Commands) -> Vec<SetMouseSettings> {
    drain(receiver)
        .into_iter()
        .filter_map(|command| match command {
            Command::SetMouseSettings(write) => Some(write),
            _ => None,
        })
        .collect()
}

#[test]
fn only_a_mouse_with_the_capability_reads_its_settings_and_only_once() {
    let (mut plain, _, mut plain_commands) = mouse_state(false);
    drain(&mut plain_commands);
    plain.load_current_mouse_settings();
    assert!(!plain.current_mouse_settings_supported());
    assert!(drain(&mut plain_commands).is_empty());

    let (mut state, key, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    state.load_current_mouse_settings();
    state.load_current_mouse_settings();
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReadMouseSettings(ReadMouseSettings { key: read, .. })] if *read == key
    ));
}

#[test]
fn an_edit_shows_at_once_and_writes_one_change() {
    let (mut state, key, mut receiver) = loaded_state();
    let change = MouseSettingsChange::LiftOff(LiftOffDistance::Mm2);

    let _ = state.commit_mouse_setting(change);

    assert_eq!(
        state.mouse_settings().map(|s| s.lift_off),
        Some(LiftOffDistance::Mm2)
    );
    let sent = writes(&mut receiver);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].key, key);
    assert_eq!(sent[0].change, change);
}

#[test]
fn nothing_is_written_before_the_settings_are_known() {
    let (mut state, _, mut receiver) = mouse_state(true);
    drain(&mut receiver);

    let _ = state.commit_mouse_setting(MouseSettingsChange::LiftOff(LiftOffDistance::Mm2));

    assert!(writes(&mut receiver).is_empty());
}

#[test]
fn only_the_newest_write_answer_replaces_the_shown_settings() {
    let (mut state, key, mut receiver) = loaded_state();
    let first = MouseSettingsChange::LiftOff(LiftOffDistance::Mm2);
    let second = MouseSettingsChange::SensorMode(SensorMode::LowPower);
    let _ = state.commit_mouse_setting(first);
    let _ = state.commit_mouse_setting(second);
    drain(&mut receiver);

    let _ = state.apply_mouse_settings_written(&key, Ok(settings().applying(&first)));
    assert_eq!(
        state.mouse_settings(),
        Some(&settings().applying(&first).applying(&second)),
        "the older answer leaves the newer edit standing"
    );

    let confirmed = settings().applying(&first).applying(&second);
    let _ = state.apply_mouse_settings_written(&key, Ok(confirmed));
    assert_eq!(state.mouse_settings(), Some(&confirmed));
}

#[test]
fn a_refused_write_reads_the_mouse_again_and_flags_an_unresponsive_one() {
    let (mut state, key, mut receiver) = loaded_state();
    let _ = state.commit_mouse_setting(MouseSettingsChange::LiftOff(LiftOffDistance::Mm2));
    drain(&mut receiver);

    let _ = state.apply_mouse_settings_written(&key, Err(WriteError::RawDeviceUnresponsive));

    assert!(state.current_mouse_unresponsive());
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReadMouseSettings(_)]
    ));
    let _ = state.apply_mouse_settings_read(&key, Ok(settings()));
    assert_eq!(state.mouse_settings(), Some(&settings()));
    assert!(!state.current_mouse_unresponsive());
}

#[test]
fn a_failed_read_offers_a_retry_instead_of_reading_forever() {
    let (mut state, key, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    state.load_current_mouse_settings();
    drain(&mut receiver);

    let _ = state.apply_mouse_settings_read(&key, Err(WriteError::RawDeviceUnresponsive));

    assert!(state.mouse_settings().is_none());
    assert!(state.mouse_settings_read_failed());
    state.load_current_mouse_settings();
    assert!(
        drain(&mut receiver).is_empty(),
        "a failure waits for the user"
    );

    let _ = state.retry_mouse_settings_read(&key);
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReadMouseSettings(_)]
    ));
    assert!(!state.mouse_settings_read_failed());
    let _ = state.apply_mouse_settings_read(&key, Ok(settings()));
    assert_eq!(state.mouse_settings(), Some(&settings()));
}
