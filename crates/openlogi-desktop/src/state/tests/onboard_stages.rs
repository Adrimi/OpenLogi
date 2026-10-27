//! Onboard DPI stages: a mouse that stores its own list shows and edits that
//! list, writes it as one replacement, and trusts only the newest read-back.

use openlogi_core::color::Rgb;
use openlogi_core::hid::{OnboardDpiStage, OnboardDpiStages, OnboardDpiStagesChange};

use super::fn_lock::drain;
use super::*;
use crate::services::ipc::{Command, ReadOnboardDpiStages, SetOnboardDpiStages};
use crate::state::onboard_stages::STAGE_COLORS;

type Commands = tokio::sync::mpsc::UnboundedReceiver<Command>;

fn stage(dpi: u16, color: Rgb) -> OnboardDpiStage {
    OnboardDpiStage {
        dpi: Dpi::new(dpi),
        color,
    }
}

fn five_stages() -> OnboardDpiStages {
    OnboardDpiStages {
        stages: vec![
            stage(800, STAGE_COLORS[0]),
            stage(1000, STAGE_COLORS[1]),
            stage(1200, STAGE_COLORS[3]),
            stage(1400, STAGE_COLORS[2]),
            stage(1600, STAGE_COLORS[4]),
        ],
        active: 3,
        max_stages: 8,
    }
}

/// The known mouse, with or without onboard DPI stages, and the channel its
/// requests go out on.
fn mouse_state(onboard: bool) -> (AppState, DeviceKey, Commands) {
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    inventory.paired[0]
        .capabilities
        .as_mut()
        .expect("the fixture mouse has capabilities")
        .onboard_dpi_stages = onboard;
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    let key = state.current_record().expect("selected").device_key();
    (state, key, receiver)
}

/// A mouse whose five stages have been read.
fn loaded_state() -> (AppState, DeviceKey, Commands) {
    let (mut state, key, mut receiver) = mouse_state(true);
    state.load_current_onboard_stages();
    let _ = state.apply_onboard_stages_read(&key, Ok(five_stages()));
    drain(&mut receiver);
    (state, key, receiver)
}

fn writes(receiver: &mut Commands) -> Vec<SetOnboardDpiStages> {
    drain(receiver)
        .into_iter()
        .filter_map(|command| match command {
            Command::SetOnboardDpiStages(write) => Some(write),
            _ => None,
        })
        .collect()
}

#[test]
fn only_a_mouse_with_the_capability_reads_its_stages_and_only_once() {
    let (mut plain, _, mut plain_commands) = mouse_state(false);
    drain(&mut plain_commands);
    plain.load_current_onboard_stages();
    assert!(!plain.current_onboard_stages_supported());
    assert!(drain(&mut plain_commands).is_empty());

    let (mut state, key, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    assert!(state.current_onboard_stages_supported());
    state.load_current_onboard_stages();
    state.load_current_onboard_stages();
    let reads = drain(&mut receiver);
    assert!(matches!(
        reads.as_slice(),
        [Command::ReadOnboardDpiStages(ReadOnboardDpiStages { key: read, .. })] if *read == key
    ));
}

#[test]
fn the_active_stage_drives_the_shown_dpi() {
    let (state, _, _receiver) = loaded_state();

    assert_eq!(state.onboard_dpi_stages(), Some(&five_stages()));
    assert_eq!(state.dpi(), Dpi::new(1400));
}

#[test]
fn selecting_a_stage_writes_the_list_with_that_stage_active_and_shows_its_dpi() {
    let (mut state, key, mut receiver) = loaded_state();

    let _ = state.select_onboard_stage(0);

    assert_eq!(state.dpi(), Dpi::new(800));
    let sent = writes(&mut receiver);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].key, key);
    assert_eq!(
        sent[0].change,
        OnboardDpiStagesChange {
            stages: five_stages().stages,
            active: 0,
        }
    );
}

#[test]
fn selecting_the_active_or_a_missing_stage_writes_nothing() {
    let (mut state, _, mut receiver) = loaded_state();

    let _ = state.select_onboard_stage(3);
    let _ = state.select_onboard_stage(5);

    assert!(writes(&mut receiver).is_empty());
}

#[test]
fn removing_a_stage_before_the_active_one_keeps_the_same_stage_active() {
    let (mut state, _, mut receiver) = loaded_state();

    let _ = state.remove_onboard_stage(1);

    let sent = writes(&mut receiver);
    let dpis: Vec<_> = sent[0].change.stages.iter().map(|s| s.dpi).collect();
    assert_eq!(dpis, [800, 1200, 1400, 1600].map(Dpi::new));
    assert_eq!(sent[0].change.active, 2, "1400 DPI is still the active one");
    assert_eq!(state.dpi(), Dpi::new(1400));
}

#[test]
fn removing_the_active_last_stage_activates_the_new_last_one() {
    let (mut state, _, mut receiver) = loaded_state();
    let _ = state.select_onboard_stage(4);
    drain(&mut receiver);

    let _ = state.remove_onboard_stage(4);

    let sent = writes(&mut receiver);
    assert_eq!(sent[0].change.stages.len(), 4);
    assert_eq!(sent[0].change.active, 3);
}

#[test]
fn the_last_stage_cannot_be_removed() {
    let (mut state, key, mut receiver) = loaded_state();
    let _ = state.apply_onboard_stages_read(
        &key,
        Ok(OnboardDpiStages {
            stages: vec![stage(800, STAGE_COLORS[0])],
            active: 0,
            max_stages: 8,
        }),
    );

    let _ = state.remove_onboard_stage(0);

    assert!(writes(&mut receiver).is_empty());
}

#[test]
fn adding_appends_the_shown_dpi_in_the_first_unused_palette_colour() {
    let (mut state, _, mut receiver) = loaded_state();
    let _ = state.set_dpi_preview(Dpi::new(3200));

    let _ = state.add_onboard_stage();

    let sent = writes(&mut receiver);
    let added = *sent[0].change.stages.last().expect("a new stage");
    assert_eq!(sent[0].change.stages.len(), 6);
    assert_eq!(added.dpi, Dpi::new(3200));
    assert_eq!(
        added.color, STAGE_COLORS[5],
        "cyan is the first colour unused"
    );
    assert_eq!(sent[0].change.active, 3, "the active stage stays");
}

#[test]
fn a_full_list_takes_no_more_stages() {
    let (mut state, key, mut receiver) = loaded_state();
    let _ = state.apply_onboard_stages_read(
        &key,
        Ok(OnboardDpiStages {
            stages: (1..=8).map(|n| stage(n * 100, STAGE_COLORS[0])).collect(),
            active: 0,
            max_stages: 8,
        }),
    );

    let _ = state.add_onboard_stage();

    assert!(writes(&mut receiver).is_empty());
}

#[test]
fn only_the_newest_write_answer_replaces_the_shown_list() {
    let (mut state, key, mut receiver) = loaded_state();
    let _ = state.select_onboard_stage(0);
    let _ = state.select_onboard_stage(1);
    drain(&mut receiver);
    let first = OnboardDpiStages {
        active: 0,
        ..five_stages()
    };
    let second = OnboardDpiStages {
        active: 1,
        ..five_stages()
    };

    let _ = state.apply_onboard_stages_written(&key, Ok(first));
    assert_eq!(state.onboard_dpi_stages().map(|s| s.active), Some(1));

    let _ = state.apply_onboard_stages_written(&key, Ok(second.clone()));
    assert_eq!(state.onboard_dpi_stages(), Some(&second));
    assert_eq!(state.dpi(), Dpi::new(1000));
}

#[test]
fn a_refused_write_reads_the_mouse_again_and_flags_an_unresponsive_one() {
    let (mut state, key, mut receiver) = loaded_state();
    let _ = state.select_onboard_stage(0);
    drain(&mut receiver);

    let _ = state.apply_onboard_stages_written(&key, Err(WriteError::RawDeviceUnresponsive));

    assert!(state.current_mouse_unresponsive());
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReadOnboardDpiStages(_)]
    ));
    let _ = state.apply_onboard_stages_read(&key, Ok(five_stages()));
    assert_eq!(state.onboard_dpi_stages(), Some(&five_stages()));
    assert_eq!(state.dpi(), Dpi::new(1400));
    assert!(!state.current_mouse_unresponsive());
}

#[test]
fn a_failed_read_offers_a_retry_instead_of_reading_forever() {
    let (mut state, key, mut receiver) = mouse_state(true);
    drain(&mut receiver);
    state.load_current_onboard_stages();
    drain(&mut receiver);

    let _ = state.apply_onboard_stages_read(&key, Err(WriteError::RawDeviceUnresponsive));

    assert!(state.onboard_dpi_stages().is_none());
    assert!(state.onboard_stages_read_failed());
    assert!(state.current_mouse_unresponsive());
    state.load_current_onboard_stages();
    assert!(
        drain(&mut receiver).is_empty(),
        "a failure waits for the user"
    );

    let _ = state.retry_onboard_stages_read(&key);
    assert!(matches!(
        drain(&mut receiver).as_slice(),
        [Command::ReadOnboardDpiStages(_)]
    ));
    assert!(!state.onboard_stages_read_failed());
    let _ = state.apply_onboard_stages_read(&key, Ok(five_stages()));
    assert_eq!(state.onboard_dpi_stages(), Some(&five_stages()));
}

#[test]
fn a_slider_write_updates_the_active_stage_in_the_list() {
    let (mut state, key, _receiver) = loaded_state();

    let _ = state.apply_dpi_written(&key, 0, Dpi::new(2000), Ok(()));

    let stages = state.onboard_dpi_stages().expect("stages");
    assert_eq!(stages.stages[3].dpi, Dpi::new(2000));
    assert_eq!(stages.stages[0].dpi, Dpi::new(800));
}
