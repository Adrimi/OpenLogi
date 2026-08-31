//! General settings page.

use super::{
    App, AppState, Entity, FluentBuilder, IconName, InteractiveElement, ParentElement,
    SettingField, SettingGroup, SettingItem, SettingPage, Slider, SliderState, StateEvent, Styled,
    ThumbwheelSensitivity, VerticalScrollSensitivity, div, h_flex, px, theme, v_flex,
};
use crate::ui::theme::Typography as _;
use gpui_base::Button as BaseButton;

use crate::platform::registration::ServiceStatus;

/// The page's two sensitivity sliders, named so a call site cannot swap two
/// same-typed `Entity<SliderState>`s without the compiler noticing.
pub(super) struct SensitivitySliders {
    pub(super) vertical_scroll: Entity<SliderState>,
    pub(super) thumbwheel: Entity<SliderState>,
}

/// What the page needs to know about the agent's login item: what launchd
/// reports, and what the user asked for. Both are needed — "start at login"
/// being on is only a promise if something is actually registered to keep it.
#[derive(Clone, Copy)]
pub(super) struct LoginItemState {
    pub(super) status: ServiceStatus,
    pub(super) launch_at_login: bool,
}

/// What the Login Items area should warn about, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginNotice {
    /// The user switched the item off under System Settings › Login Items.
    /// macOS is overriding the switch on this page.
    NeedsApproval,
    /// The switch says "start at login" but nothing is registered to do it.
    /// A reboot then leaves the agent — and every binding — down, with the
    /// switch still claiming otherwise.
    NotRegistered,
}

/// Decide which notice the Login Items area owes the user.
///
/// `RequiresApproval` is reported whatever the switch says: macOS is
/// overriding it either way. The unregistered states are only a problem when
/// the user actually asked to start at login.
fn login_notice(state: LoginItemState) -> Option<LoginNotice> {
    match state.status {
        ServiceStatus::RequiresApproval => Some(LoginNotice::NeedsApproval),
        ServiceStatus::NotRegistered | ServiceStatus::NotFound if state.launch_at_login => {
            Some(LoginNotice::NotRegistered)
        }
        _ => None,
    }
}

pub(super) fn general_page(sliders: SensitivitySliders, login_item: LoginItemState) -> SettingPage {
    let SensitivitySliders {
        vertical_scroll,
        thumbwheel,
    } = sliders;
    let group = SettingGroup::new()
        .item(smooth_scrolling_item())
        .item(
            SettingItem::new(
                tr!("Vertical Scroll Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    vertical_scroll_sensitivity_field(&vertical_scroll, cx)
                }),
            )
            .description(tr!(
                "Scales traditional mouse-wheel vertical distance without changing trackpad scrolling."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Thumb Wheel Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    thumbwheel_sensitivity_field(&thumbwheel, cx)
                }),
            )
            .description(tr!(
                "Scales the thumb wheel's horizontal scroll speed and how readily custom wheel actions trigger."
            )),
        )
        .item(launch_at_login_item());

    // The switch above is a preference, not a guarantee: it is the launchd
    // registration that actually starts the agent. Say so when the two
    // disagree, rather than letting the switch claim a state the system is
    // not honouring.
    let group = match login_notice(login_item) {
        Some(LoginNotice::NeedsApproval) => group.item(login_item_approval_notice()),
        Some(LoginNotice::NotRegistered) => group.item(login_item_missing_notice()),
        None => group,
    };

    // One `show_in_menu_bar` setting drives the macOS status item and the
    // Windows notification-area icon (honored at next agent launch); Linux
    // has no tray, so no switch.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let group = group.item(
        SettingItem::new(
            if cfg!(target_os = "macos") {
                tr!("Show in menu bar")
            } else {
                tr!("Show in the notification area")
            },
            SettingField::switch(
                |cx| {
                    AppState::try_read(cx)
                        .is_some_and(|s| s.app_settings().show_in_menu_bar)
                },
                |enabled, cx| {
                    AppState::update(cx, move |state, cx| {
                        state.set_show_in_menu_bar(enabled);
                        cx.emit(StateEvent::SettingsChanged);
                    });
                },
            ),
        )
        .description(if cfg!(target_os = "macos") {
            tr!("Keep OpenLogi's icon in the menu bar. When off, it stays in the Dock instead.")
        } else {
            tr!(
                "Keep OpenLogi's icon in the taskbar notification area. Takes effect the next time the background agent starts."
            )
        }),
    );

    SettingPage::new(tr!("General"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

/// The smooth-scrolling switch.
fn smooth_scrolling_item() -> SettingItem {
    SettingItem::new(
        tr!("Smooth scrolling"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().smooth_scroll),
            |enabled, cx| {
                AppState::update(cx, move |state, cx| {
                    state.set_smooth_scroll(enabled);
                    cx.emit(StateEvent::SettingsChanged);
                });
            },
        ),
    )
    .description(tr!(
        "Animate traditional mouse-wheel input while leaving trackpad scrolling unchanged."
    ))
}

fn thumbwheel_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> gpui::Div {
    let value = ThumbwheelSensitivity::from_rounded(slider.read(cx).value().start());
    sensitivity_field(
        slider,
        value.to_string(),
        value == ThumbwheelSensitivity::DEFAULT,
        cx,
    )
}

fn vertical_scroll_sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> gpui::Div {
    let value = VerticalScrollSensitivity::from_rounded(slider.read(cx).value().start());
    sensitivity_field(
        slider,
        value.to_string(),
        value == VerticalScrollSensitivity::DEFAULT,
        cx,
    )
}

fn sensitivity_field(
    slider: &Entity<SliderState>,
    value: String,
    is_default: bool,
    cx: &mut App,
) -> gpui::Div {
    let pal = theme::palette(cx);
    v_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(div().w(px(180.)).child(Slider::new(slider)))
                .child(
                    div()
                        .w(px(72.))
                        .text_body()
                        .text_color(pal.text_muted)
                        .child(value),
                ),
        )
        .when(is_default, |this| {
            this.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .whitespace_nowrap()
                    .child(format!("({})", rust_i18n::t!("Default"))),
            )
        })
}

/// The launch-at-login switch — a persisted config value the agent reads
/// (the sunk switch); the setter never unregisters.
fn launch_at_login_item() -> SettingItem {
    SettingItem::new(
        tr!("Launch at login"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().launch_at_login),
            |enabled, cx| {
                AppState::update(cx, move |state, cx| {
                    state.set_launch_at_login(enabled);
                    cx.emit(StateEvent::SettingsChanged);
                });
            },
        ),
    )
    .description(if cfg!(target_os = "macos") {
        tr!("Automatically start OpenLogi when you log in to macOS.")
    } else {
        tr!("Automatically start OpenLogi when you log in.")
    })
}

/// The `RequiresApproval` notice: with the direct-launch fallback gone, the
/// switched-off login item stops the agent entirely, whatever the preference.
fn login_item_approval_notice() -> SettingItem {
    SettingItem::new(
        tr!("Login item disabled in System Settings"),
        SettingField::render(|_, _, cx| open_login_items_button(cx)),
    )
    .description(tr!(
        "macOS is blocking OpenLogi's background agent: its login item is switched off. The agent cannot run until you turn it back on under Login Items."
    ))
}

/// "Start at login" is on, but nothing is registered to honour it — the shape
/// that leaves every binding dead after a reboot while the switch still reads
/// as enabled.
fn login_item_missing_notice() -> SettingItem {
    SettingItem::new(
        tr!("Not registered to start at login"),
        SettingField::render(|_, _, cx| register_login_item_button(cx)),
    )
    .description(tr!(
        "OpenLogi is set to start at login, but its background agent is not registered with macOS, so a restart leaves it — and your key bindings — switched off until you open the app."
    ))
}

/// Retry the registration the startup path could not complete. The row
/// refreshes on the next window activation, which is what already re-reads
/// the status after a trip to System Settings.
fn register_login_item_button(cx: &App) -> BaseButton {
    let pal = theme::palette(cx);
    BaseButton::new("register-login-item")
        .accessibility_label(tr!("Register"))
        .px_2()
        .py_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_caption()
        .cursor_pointer()
        .bg(pal.control)
        .hover(move |s| s.bg(pal.control_hover))
        .focus_visible(move |s| s.bg(pal.control_hover))
        .child(tr!("Register"))
        .on_click(|_, _, _| {
            if let Err(error) = crate::platform::registration::ensure_registered() {
                tracing::warn!(error, "manual login-item registration failed");
            }
        })
}

/// Deep link to System Settings › Login Items — the only place that can
/// re-enable a service switched off there.
fn open_login_items_button(cx: &App) -> BaseButton {
    let pal = theme::palette(cx);
    BaseButton::new("open-login-items")
        .accessibility_label(tr!("Open Login Items"))
        .px_2()
        .py_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_caption()
        .cursor_pointer()
        .bg(pal.control)
        .hover(move |s| s.bg(pal.control_hover))
        .focus_visible(move |s| s.bg(pal.control_hover))
        .child(tr!("Open Login Items"))
        .on_click(|_, _, _| crate::platform::registration::open_login_items_settings())
}

#[cfg(test)]
mod tests {
    use super::{LoginItemState, LoginNotice, ServiceStatus, login_notice};

    fn state(status: ServiceStatus, launch_at_login: bool) -> LoginItemState {
        LoginItemState {
            status,
            launch_at_login,
        }
    }

    #[test]
    fn a_registered_service_needs_no_notice() {
        assert_eq!(login_notice(state(ServiceStatus::Enabled, true)), None);
    }

    #[test]
    fn wanting_login_start_without_a_registration_is_surfaced() {
        // The reboot case: the switch reads "on", nothing is registered, and
        // without this the user only finds out because their keyboard stopped
        // working.
        assert_eq!(
            login_notice(state(ServiceStatus::NotRegistered, true)),
            Some(LoginNotice::NotRegistered)
        );
        assert_eq!(
            login_notice(state(ServiceStatus::NotFound, true)),
            Some(LoginNotice::NotRegistered)
        );
    }

    #[test]
    fn not_wanting_login_start_makes_an_absent_registration_expected() {
        assert_eq!(
            login_notice(state(ServiceStatus::NotRegistered, false)),
            None
        );
    }

    #[test]
    fn an_override_in_system_settings_is_surfaced_whatever_the_switch_says() {
        // macOS wins either way, so this one does not consult the preference.
        for launch_at_login in [true, false] {
            assert_eq!(
                login_notice(state(ServiceStatus::RequiresApproval, launch_at_login)),
                Some(LoginNotice::NeedsApproval)
            );
        }
    }
}
