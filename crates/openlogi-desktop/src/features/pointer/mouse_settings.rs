//! The Sensor and Lighting cards of a mouse that stores its own settings.
//!
//! Both cards edit [`MouseSettings`] through
//! [`AppState::commit_mouse_setting`]: a pick shows at once and is written to
//! the mouse, whose read-back replaces it. Until the mouse has answered a
//! read, the cards say so, or offer a retry when the read failed.

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, rgb,
};
use gpui_kit::component::{Disableable as _, Icon, Selectable as _, h_flex, v_flex};
use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    AngleAdjustment, BodyLight, BodyLightMode, ButtonDebounce, DpiLed, DpiLedEffect, IdleTimeout,
    Level, LiftOffDistance, MouseSettings, MouseSettingsChange, SensorMode, SensorSwitch,
    WheelDebounce,
};

use crate::state::{AppState, DeviceKey, STAGE_COLORS};
use crate::ui::components::{PanelCard, Pill, Toggle, control_button, toggle_row};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{Palette, Typography as _};

/// What the cards show about the selected device.
pub(crate) struct MouseSettingsFacts {
    key: DeviceKey,
    settings: Option<MouseSettings>,
    read_failed: bool,
}

impl MouseSettingsFacts {
    /// The selected device's facts, or `None` when it stores no settings or is
    /// offline (the DPI row already says so).
    pub(crate) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_mouse_settings_supported() {
            return None;
        }
        let record = state.current_record()?;
        record.route.as_ref()?;
        Some(Self {
            key: record.device_key(),
            settings: state.mouse_settings().copied(),
            read_failed: state.mouse_settings_read_failed(),
        })
    }
}

/// The Sensor card, or `None` for a mouse without stored settings.
pub(crate) fn sensor_card(facts: Option<&MouseSettingsFacts>, pal: Palette) -> Option<PanelCard> {
    let facts = facts?;
    Some(PanelCard::new(
        tr!("pointer.sensor"),
        Icon::empty().path("action-icons/gauge.svg"),
        card_body(facts, pal, sensor_rows),
    ))
}

/// The Lighting card, or `None` for a mouse without stored settings.
pub(crate) fn lighting_card(facts: Option<&MouseSettingsFacts>, pal: Palette) -> Option<PanelCard> {
    let facts = facts?;
    Some(PanelCard::new(
        tr!("pointer.mouse_lighting"),
        Icon::empty().path("action-icons/palette.svg"),
        card_body(facts, pal, lighting_rows),
    ))
}

fn card_body(
    facts: &MouseSettingsFacts,
    pal: Palette,
    rows: fn(MouseSettings, Palette) -> AnyElement,
) -> AnyElement {
    match facts.settings {
        Some(settings) => rows(settings, pal),
        None if facts.read_failed => {
            let key = facts.key.clone();
            retry_line(
                "mouse-settings-retry",
                tr!("pointer.couldnt_read_mouse_settings_click_to_retry"),
                move |cx| {
                    AppState::apply(cx, |state| state.retry_mouse_settings_read(&key));
                },
            )
            .into_any_element()
        }
        None => status_line(tr!("pointer.reading_mouse_settings"), pal).into_any_element(),
    }
}

fn commit(change: MouseSettingsChange, cx: &mut App) {
    AppState::apply(cx, |state| state.commit_mouse_setting(change));
}

fn sensor_rows(settings: MouseSettings, pal: Palette) -> AnyElement {
    v_flex()
        .gap_4()
        .w_full()
        .child(sensor_tracking_rows(settings, pal))
        .child(sensor_timing_rows(settings, pal))
        .into_any_element()
}

/// Lift-off, sensor mode, the sensor switches and the angle adjustment.
fn sensor_tracking_rows(settings: MouseSettings, pal: Palette) -> AnyElement {
    let lift_off = LiftOffDistance::ALL.into_iter().map(|distance| {
        let label = match distance {
            LiftOffDistance::Mm0Point7 => "0.7",
            LiftOffDistance::Mm1 => "1",
            LiftOffDistance::Mm2 => "2",
        };
        (
            ("lift-off", distance as usize),
            tr!("pointer.distance_mm", value => label),
            distance == settings.lift_off,
            MouseSettingsChange::LiftOff(distance),
        )
    });
    let sensor_mode = SensorMode::ALL.into_iter().map(|mode| {
        (
            ("sensor-mode", mode as usize),
            match mode {
                SensorMode::LowPower => tr!("pointer.sensor_mode_low_power"),
                SensorMode::HighPerformance => tr!("pointer.sensor_mode_high_performance"),
            },
            mode == settings.sensor_mode,
            MouseSettingsChange::SensorMode(mode),
        )
    });
    let switches = SensorSwitch::ALL.into_iter().map(|switch| {
        let (title, description) = switch_texts(switch);
        toggle_row(
            switch_selector(switch),
            pal,
            title,
            description,
            Toggle::new(("sensor-switch", switch as usize))
                .selected(settings.switches.is_on(switch))
                .on_change(move |on, _window, cx| {
                    commit(MouseSettingsChange::Switch { switch, on: *on }, cx);
                }),
        )
    });
    let angle = settings.angle_adjustment.degrees();
    v_flex()
        .gap_4()
        .w_full()
        .child(choice_row(
            tr!("pointer.lift_off_distance"),
            tr!("pointer.lift_off_distance_description"),
            pills(lift_off),
            pal,
        ))
        .child(choice_row(
            tr!("pointer.sensor_mode"),
            tr!("pointer.sensor_mode_description"),
            pills(sensor_mode),
            pal,
        ))
        .children(switches)
        .child(choice_row(
            tr!("pointer.angle_adjustment"),
            tr!("pointer.angle_adjustment_description"),
            stepper(
                "angle-adjustment",
                tr!("pointer.angle_degrees", degrees => angle),
                AngleAdjustment::new(angle.saturating_sub(1))
                    .ok()
                    .map(MouseSettingsChange::AngleAdjustment),
                AngleAdjustment::new(angle.saturating_add(1))
                    .ok()
                    .map(MouseSettingsChange::AngleAdjustment),
                pal,
            ),
            pal,
        ))
        .into_any_element()
}

/// Sleep timeout and the button and wheel debounce.
fn sensor_timing_rows(settings: MouseSettings, pal: Palette) -> AnyElement {
    let idle = IdleTimeout::ALL.into_iter().map(|timeout| {
        let label = if timeout.seconds() < 60 {
            tr!("pointer.idle_seconds", value => timeout.seconds())
        } else {
            tr!("pointer.idle_minutes", value => timeout.seconds() / 60)
        };
        (
            ("idle-timeout", timeout as usize),
            label,
            timeout == settings.idle_timeout,
            MouseSettingsChange::IdleTimeout(timeout),
        )
    });
    let wheel = (WheelDebounce::MIN_MS..=WheelDebounce::MAX_MS)
        .step_by(usize::from(WheelDebounce::STEP_MS))
        .filter_map(|milliseconds| WheelDebounce::new(milliseconds).ok())
        .map(|debounce| {
            (
                ("wheel-debounce", usize::from(debounce.milliseconds())),
                tr!("pointer.milliseconds", value => debounce.milliseconds()),
                settings.wheel_debounce == Some(debounce),
                MouseSettingsChange::WheelDebounce(debounce),
            )
        });
    let debounce = settings.button_debounce.milliseconds();
    v_flex()
        .gap_4()
        .w_full()
        .child(choice_row(
            tr!("pointer.idle_timeout"),
            tr!("pointer.idle_timeout_description"),
            pills(idle),
            pal,
        ))
        .child(choice_row(
            tr!("pointer.button_debounce"),
            tr!("pointer.button_debounce_description"),
            stepper(
                "button-debounce",
                tr!("pointer.milliseconds", value => debounce),
                ButtonDebounce::new(debounce.saturating_sub(1))
                    .ok()
                    .filter(|_| debounce > 0)
                    .map(MouseSettingsChange::ButtonDebounce),
                ButtonDebounce::new(debounce.saturating_add(1))
                    .ok()
                    .map(MouseSettingsChange::ButtonDebounce),
                pal,
            ),
            pal,
        ))
        .child(choice_row(
            tr!("pointer.wheel_debounce"),
            tr!("pointer.wheel_debounce_description"),
            pills(wheel),
            pal,
        ))
        .into_any_element()
}

fn switch_selector(switch: SensorSwitch) -> &'static str {
    match switch {
        SensorSwitch::MotionSync => "motion-sync-row",
        SensorSwitch::AngleSnapping => "angle-snapping-row",
        SensorSwitch::RippleControl => "ripple-control-row",
        SensorSwitch::HighestPerformance => "highest-performance-row",
        SensorSwitch::TwentyKFps => "fps-20k-row",
    }
}

fn switch_texts(switch: SensorSwitch) -> (SharedString, SharedString) {
    match switch {
        SensorSwitch::MotionSync => (
            tr!("pointer.motion_sync"),
            tr!("pointer.motion_sync_description"),
        ),
        SensorSwitch::AngleSnapping => (
            tr!("pointer.angle_snapping"),
            tr!("pointer.angle_snapping_description"),
        ),
        SensorSwitch::RippleControl => (
            tr!("pointer.ripple_control"),
            tr!("pointer.ripple_control_description"),
        ),
        SensorSwitch::HighestPerformance => (
            tr!("pointer.highest_performance"),
            tr!("pointer.highest_performance_description"),
        ),
        SensorSwitch::TwentyKFps => (tr!("pointer.fps_20k"), tr!("pointer.fps_20k_description")),
    }
}

fn lighting_rows(settings: MouseSettings, pal: Palette) -> AnyElement {
    v_flex()
        .gap_6()
        .w_full()
        .child(dpi_led_section(settings.dpi_led, pal))
        .child(body_light_section(settings.body_light, pal))
        .into_any_element()
}

fn dpi_led_section(led: DpiLed, pal: Palette) -> AnyElement {
    let led_effect = [None]
        .into_iter()
        .chain(DpiLedEffect::ALL.map(Some))
        .map(|effect| {
            let (id, label, selected, next) = match effect {
                None => (
                    0,
                    tr!("common.off"),
                    !led.enabled,
                    DpiLed {
                        enabled: false,
                        ..led
                    },
                ),
                Some(effect) => (
                    1 + effect as usize,
                    match effect {
                        DpiLedEffect::Steady => tr!("pointer.led_steady"),
                        DpiLedEffect::Breathing => tr!("pointer.led_breathing"),
                    },
                    led.enabled && led.effect == effect,
                    DpiLed {
                        effect,
                        enabled: true,
                        ..led
                    },
                ),
            };
            (
                ("dpi-led-effect", id),
                label,
                selected,
                MouseSettingsChange::DpiLed(next),
            )
        });
    v_flex()
        .gap_4()
        .child(choice_row(
            tr!("pointer.dpi_led"),
            tr!("pointer.dpi_led_description"),
            pills(led_effect),
            pal,
        ))
        .children(led.enabled.then(|| {
            v_flex()
                .gap_4()
                .child(choice_row(
                    tr!("pointer.light_brightness"),
                    SharedString::default(),
                    level_pills("dpi-led-brightness", led.brightness, move |brightness| {
                        MouseSettingsChange::DpiLed(DpiLed { brightness, ..led })
                    }),
                    pal,
                ))
                .children((led.effect == DpiLedEffect::Breathing).then(|| {
                    choice_row(
                        tr!("pointer.light_speed"),
                        SharedString::default(),
                        level_pills("dpi-led-speed", led.speed, move |speed| {
                            MouseSettingsChange::DpiLed(DpiLed { speed, ..led })
                        }),
                        pal,
                    )
                }))
        }))
        .into_any_element()
}

fn body_light_section(light: BodyLight, pal: Palette) -> AnyElement {
    let body_mode = [BodyLightMode::Off]
        .into_iter()
        .chain(BodyLightMode::LIT)
        .map(|mode| {
            let label = match mode {
                BodyLightMode::Off => tr!("common.off"),
                BodyLightMode::Rainbow => tr!("pointer.body_light_rainbow"),
                BodyLightMode::ColorBreath => tr!("pointer.body_light_color_breath"),
                BodyLightMode::FixedColor => tr!("pointer.body_light_fixed_color"),
                BodyLightMode::Neon => tr!("pointer.body_light_neon"),
                BodyLightMode::RainbowBreath => tr!("pointer.body_light_rainbow_breath"),
                BodyLightMode::FixedRainbow => tr!("pointer.body_light_fixed_rainbow"),
            };
            let selected = if mode == BodyLightMode::Off {
                !light.is_lit()
            } else {
                light.is_lit() && light.mode == mode
            };
            (
                ("body-light-mode", mode as usize),
                label,
                selected,
                MouseSettingsChange::BodyLight(light.selecting(mode)),
            )
        });

    v_flex()
        .gap_4()
        .child(choice_row(
            tr!("pointer.body_light"),
            tr!("pointer.body_light_description"),
            pills(body_mode),
            pal,
        ))
        .children(light.is_lit().then(|| {
            v_flex()
                .gap_4()
                .children(light.mode.uses_color().then(|| {
                    choice_row(
                        tr!("pointer.body_light_color"),
                        SharedString::default(),
                        swatches(light, pal),
                        pal,
                    )
                }))
                .child(choice_row(
                    tr!("pointer.light_brightness"),
                    SharedString::default(),
                    level_pills(
                        "body-light-brightness",
                        light.brightness,
                        move |brightness| {
                            MouseSettingsChange::BodyLight(BodyLight {
                                brightness,
                                ..light
                            })
                        },
                    ),
                    pal,
                ))
                .child(choice_row(
                    tr!("pointer.light_speed"),
                    SharedString::default(),
                    level_pills("body-light-speed", light.speed, move |speed| {
                        MouseSettingsChange::BodyLight(BodyLight { speed, ..light })
                    }),
                    pal,
                ))
                .child(toggle_row(
                    "body-light-off-while-moving-row",
                    pal,
                    tr!("pointer.body_light_off_while_moving"),
                    tr!("pointer.body_light_off_while_moving_description"),
                    Toggle::new("body-light-off-while-moving")
                        .selected(light.off_while_moving)
                        .on_change(move |on, _window, cx| {
                            commit(
                                MouseSettingsChange::BodyLight(BodyLight {
                                    off_while_moving: *on,
                                    ..light
                                }),
                                cx,
                            );
                        }),
                ))
        }))
        .into_any_element()
}

/// A title and caption above a control.
fn choice_row(
    title: SharedString,
    description: SharedString,
    control: AnyElement,
    pal: Palette,
) -> AnyElement {
    v_flex()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_body().text_color(pal.text_primary).child(title))
                .children((!description.is_empty()).then(|| {
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(description)
                })),
        )
        .child(control)
        .into_any_element()
}

type PillSpec = (
    (&'static str, usize),
    SharedString,
    bool,
    MouseSettingsChange,
);

/// A wrapping row of pills; clicking one commits its change.
fn pills(specs: impl IntoIterator<Item = PillSpec>) -> AnyElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(specs.into_iter().map(|(id, label, selected, change)| {
            Pill::new(id, label)
                .selected(selected)
                .on_click(move |_event, _window, cx| commit(change, cx))
        }))
        .into_any_element()
}

/// The ten positions of a slider as pills.
fn level_pills(
    id: &'static str,
    current: Level,
    change: impl Fn(Level) -> MouseSettingsChange + Copy + 'static,
) -> AnyElement {
    pills(
        (Level::MIN.get()..=Level::MAX.get()).filter_map(move |step| {
            let level = Level::new(step).ok()?;
            Some((
                (id, usize::from(step)),
                SharedString::from(step.to_string()),
                level == current,
                change(level),
            ))
        }),
    )
}

/// The value between a minus and a plus button; a missing change disables its
/// button.
fn stepper(
    id: &'static str,
    value: SharedString,
    minus: Option<MouseSettingsChange>,
    plus: Option<MouseSettingsChange>,
    pal: Palette,
) -> AnyElement {
    let step = |slot: usize, label: &'static str, change: Option<MouseSettingsChange>| {
        let button = control_button((id, slot))
            .label(label)
            .outline()
            .disabled(change.is_none());
        match change {
            Some(change) => button.on_click(move |_event, _window, cx| commit(change, cx)),
            None => button,
        }
    };
    h_flex()
        .gap_3()
        .items_center()
        .child(step(0, "−", minus))
        .child(
            div()
                .min_w(px(56.))
                .text_body()
                .text_color(pal.text_primary)
                .child(value),
        )
        .child(step(1, "+", plus))
        .into_any_element()
}

/// The palette of colours the body light offers, as round swatches.
fn swatches(light: BodyLight, pal: Palette) -> AnyElement {
    h_flex()
        .gap_2()
        .flex_wrap()
        .children(
            STAGE_COLORS
                .into_iter()
                .enumerate()
                .map(|(index, color)| swatch(index, color, light.color == color, light, pal)),
        )
        .into_any_element()
}

fn swatch(
    index: usize,
    color: Rgb,
    selected: bool,
    light: BodyLight,
    pal: Palette,
) -> impl IntoElement {
    div()
        .id(("body-light-color", index))
        .size(px(22.))
        .flex_none()
        .rounded_full()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            pal.text_primary
        } else {
            pal.border
        })
        .bg(rgb(color.packed()))
        .on_click(move |_event, _window, cx| {
            commit(
                MouseSettingsChange::BodyLight(BodyLight { color, ..light }),
                cx,
            );
        })
}
