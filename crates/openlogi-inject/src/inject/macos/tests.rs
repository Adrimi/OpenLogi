use core_graphics::event::{CGEventFlags, EventField};
use openlogi_core::binding::{MediaKey, Shortcut};

use super::{
    LOCK_SCREEN_CHORD, NX_KEYTYPE_BRIGHTNESS_DOWN, NX_KEYTYPE_BRIGHTNESS_UP, NX_KEYTYPE_SOUND_UP,
    combo, held_key_event, hid_usage_to_macos, key_event, nx_key,
};
use crate::inject::{HeldKey, HeldModifiers, KeyPhase, parse_shortcut};

#[test]
fn hid_usages_map_to_macos_virtual_keys() {
    assert_eq!(hid_usage_to_macos(0x04), Some(0x00));
    assert_eq!(hid_usage_to_macos(0x13), Some(0x23));
    assert_eq!(hid_usage_to_macos(0x50), Some(0x7b));
    assert_eq!(hid_usage_to_macos(0x3a), Some(0x7a));
    assert_eq!(hid_usage_to_macos(0x6f), Some(0x5a));
    assert_eq!(hid_usage_to_macos(0xff), None);
}

#[test]
fn display_brightness_maps_to_native_system_keys() {
    assert_eq!(nx_key(MediaKey::BrightnessUp), NX_KEYTYPE_BRIGHTNESS_UP);
    assert_eq!(nx_key(MediaKey::BrightnessDown), NX_KEYTYPE_BRIGHTNESS_DOWN);
}

#[test]
fn volume_up_maps_to_the_native_system_key() {
    assert_eq!(nx_key(MediaKey::VolumeUp), NX_KEYTYPE_SOUND_UP);
}

/// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
/// edit to the table can't silently change what ⌘C sends. macOS and
/// Linux only overlap on the letter-key chords: both `BrowserBack` and
/// `Redo` differ across the three backends by design (see the module
/// doc on `combo`), so each backend pins its own rows independently.
#[test]
fn combo_table_pins_representative_shortcuts() {
    assert_eq!(combo(Shortcut::Copy).rendered_label(), "Cmd+C");
    assert_eq!(combo(Shortcut::Redo).rendered_label(), "Cmd+Shift+Z");
    assert_eq!(combo(Shortcut::BrowserBack).rendered_label(), "Cmd+[");
    assert_eq!(combo(Shortcut::NextTab).rendered_label(), "Ctrl+Tab");
    // hid_usage_to_macos must actually resolve every table entry, or a
    // `Shortcut` silently no-ops instead of pressing anything (see
    // `press_combo`'s warn-and-drop path). Iterates `Shortcut::ALL`
    // rather than a hand-copied list, so a newly added `Shortcut`
    // variant is checked here automatically instead of depending on
    // someone remembering to extend a second, independent list.
    for &shortcut in Shortcut::ALL {
        let key = combo(shortcut).key().code();
        assert!(
            hid_usage_to_macos(key).is_some(),
            "{shortcut:?} table entry has no macOS virtual-key mapping"
        );
    }
}

/// Lock Screen is the one chord outside the `combo` table, so the table loop
/// above never parses it. Pin it the same way: it must parse to the chord the
/// Apple menu shows, and its key must resolve to a macOS virtual key.
#[test]
fn lock_screen_chord_parses_and_resolves() {
    let chord = parse_shortcut(LOCK_SCREEN_CHORD);
    assert_eq!(chord.rendered_label(), "Cmd+Ctrl+Q");
    assert!(
        hid_usage_to_macos(chord.key().code()).is_some(),
        "the Lock Screen chord has no macOS virtual-key mapping"
    );
}

#[test]
fn held_edges_carry_the_aggregate_modifier_state() {
    let mut modifiers = HeldModifiers::default();
    let (_, flags) = held_key_event(HeldKey::Command, KeyPhase::Down, &mut modifiers)
        .expect("Command has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));

    let (_, flags) = held_key_event(HeldKey::Control, KeyPhase::Down, &mut modifiers)
        .expect("Control has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));

    let key = combo(Shortcut::Copy).key();
    let (_, flags) = held_key_event(HeldKey::Key(key), KeyPhase::Up, &mut modifiers)
        .expect("Copy's key has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));

    let (_, flags) = held_key_event(HeldKey::Command, KeyPhase::Up, &mut modifiers)
        .expect("Command has a macOS virtual-key mapping");
    assert!(!flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));
}

/// The hook skips a key edge only by this tag, so every edge the backend
/// posts must carry it.
#[test]
fn posted_key_edges_carry_the_synthetic_tag() {
    for phase in [KeyPhase::Down, KeyPhase::Up] {
        let event =
            key_event(0x0c, CGEventFlags::CGEventFlagCommand, phase).expect("a keyboard event");
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            crate::SYNTHETIC_EVENT_USER_DATA
        );
    }
}
