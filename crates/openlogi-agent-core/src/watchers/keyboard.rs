//! Background HID++ key-capture watcher for a bound keyboard.
//!
//! Runs [`openlogi_hid::run_keyboard_capture_session_with_registry`] on a
//! dedicated thread for the keyboard the orchestrator publishes in
//! [`SharedKeyboardSpec`], restarts it when the keyboard (or the set of bound
//! keys) changes, and dispatches each captured key press through the common
//! action path ([`crate::runtime::ActionDispatcher`]).
//!
//! The mouse capture watcher ([`super::gesture`]) and this one hold *shared*
//! receiver leases, so both run concurrently; pairing still waits for (and
//! excludes) both. Like the gesture watcher, this needs no macOS Accessibility
//! permission — the key events arrive over HID++.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_core::binding::{Binding, ButtonId};
use openlogi_hid::{
    CaptureChannel, CapturedInput, ChannelRegistry, DeviceRoute,
    run_keyboard_capture_session_with_registry,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::gesture::DoneAction;
use crate::receiver_access::ReceiverAccess;
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert (only keys carrying a real binding), and the
/// per-key action map presses dispatch through. Rebuilt by the orchestrator on
/// config / inventory / foreground-app changes.
#[derive(Clone)]
pub struct KeyboardSpec {
    /// Stable config key used to scope lifecycle cancellation and hardware
    /// actions to this keyboard.
    pub config_key: String,
    /// HID++ route of the keyboard.
    pub route: DeviceRoute,
    /// Physical F-key position → button, for exactly the bound keys.
    pub wanted: BTreeMap<u8, ButtonId>,
    /// Effective per-key immediate or threshold map (per-app overlay applied).
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Capture re-arm generation, bumped by the orchestrator when a device
    /// power-cycles (reconnect, replug, system wake). Part of the session
    /// identity so the same keyboard on the same route still restarts its
    /// capture: the gesture watcher has always honoured this, and a keyboard
    /// that came back from a nap used to keep a session pointed at a channel
    /// the device no longer answers on.
    pub rearm_generation: u64,
}

/// Shared keyboard-capture spec, `None` when no online keyboard has bound
/// keys. Written by the orchestrator, read by the watcher.
pub type SharedKeyboardSpec = Arc<RwLock<Option<KeyboardSpec>>>;

/// Capture identity excluding bindings, which may change without requiring a
/// hardware session restart when the diverted key set stays the same.
#[derive(Clone, PartialEq)]
struct KeyboardTarget {
    config_key: String,
    route: DeviceRoute,
    wanted: BTreeMap<u8, ButtonId>,
    rearm_generation: u64,
}

impl KeyboardTarget {
    fn for_spec(spec: KeyboardSpec) -> Self {
        Self {
            config_key: spec.config_key,
            route: spec.route,
            wanted: spec.wanted,
            rearm_generation: spec.rearm_generation,
        }
    }

    fn matches(&self, spec: &KeyboardSpec) -> bool {
        self.config_key == spec.config_key && self.route == spec.route && self.wanted == spec.wanted
    }
}

struct RunningKeyboardSession {
    id: HidppSessionId,
    target: KeyboardTarget,
    /// Present while the session runs; taken to request a stop. `None` means
    /// the session is draining — deliberately stopped, but its task (and the
    /// control-restore writes in its teardown) may still be in flight.
    stop: Option<oneshot::Sender<()>>,
}

/// Decide the [`DoneAction`] for a completion report, given the session the
/// manager currently tracks. The gesture manager's rule, applied to the
/// single keyboard slot: only the current session's report settles anything;
/// one whose stop sender is gone was stopped deliberately and merely frees
/// the slot, while one still holding it exited on its own and warrants a
/// warning alongside the re-arm.
fn on_done(done_session: &HidppSessionId, live: Option<&RunningKeyboardSession>) -> DoneAction {
    match live {
        Some(session) if session.id == *done_session => DoneAction::Remove {
            unexpected: session.stop.is_some(),
        },
        _ => DoneAction::Ignore,
    }
}

/// Whether an input belongs to the current, still-live session. A draining
/// session has already had its presses cancelled, so even its correctly
/// tagged queued events must not enter the replacement lifecycle.
fn accepts_input(input_session: &HidppSessionId, live: Option<&RunningKeyboardSession>) -> bool {
    live.is_some_and(|session| session.id == *input_session && session.stop.is_some())
}

struct KeyboardInput {
    session: HidppSessionId,
    input: CapturedInput,
}

/// How often to re-read the spec so a config edit, per-app overlay change, or
/// keyboard reconnect re-points the capture session.
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Spawn the keyboard-capture manager thread. It owns a current-thread tokio
/// runtime that keeps one capture session pointed at the bound keyboard and
/// dispatches each captured key press.
pub fn spawn(
    spec: SharedKeyboardSpec,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "keyboard watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(
            spec,
            keyboard_channel,
            receiver_access,
            registry,
            dispatcher,
        ));
    });
}

/// Route one accepted keyboard edge through the shared HID++ lifecycle.
fn dispatch_input(
    session: &HidppSessionId,
    input: CapturedInput,
    spec: &KeyboardSpec,
    dispatcher: &ActionDispatcher,
) {
    match input {
        CapturedInput::ButtonDown(button) => {
            let binding = spec.bindings.get(&button);
            if let Some(binding) = binding {
                info!(button = %button, action = %binding.click_action().label(), "keyboard key → handling binding");
            } else {
                debug!(?button, "keyboard key with no binding — ignored");
            }
            dispatcher.try_hidpp_button_down(session, button, binding);
        }
        CapturedInput::ButtonUp(button) => {
            dispatcher.try_hidpp_button_up(session, button);
        }
        CapturedInput::ButtonPulse(button) => {
            dispatcher.dispatch_hidpp_button_pulse(session, button, spec.bindings.get(&button));
        }
        CapturedInput::Gesture(..) | CapturedInput::Scroll { .. } => {}
    }
}

/// Snapshot the keyboard session target unless pairing currently owns capture.
/// What the watcher should be doing this tick.
enum Wanted {
    /// Capture this keyboard.
    Session(KeyboardTarget),
    /// Capture nothing, for this reason.
    Idle(IdleReason),
}

impl Wanted {
    /// The target to compare a running session against, if any.
    fn target(&self) -> Option<&KeyboardTarget> {
        match self {
            Self::Session(target) => Some(target),
            Self::Idle(_) => None,
        }
    }
}

/// What this tick should do with the session already running.
#[derive(Debug, PartialEq, Eq)]
enum SessionAction {
    /// The running session still captures exactly what is wanted.
    Keep,
    /// Stop it; the next tick arms a replacement (or stays idle).
    Restart,
}

/// Decide whether the running session still matches what is wanted.
///
/// The identity deliberately includes the capture re-arm generation, so a
/// keyboard that power-cycled — same device, same route, same bound keys —
/// still restarts. Its diverted controls are gone after the power cycle, and
/// a session pointed at the pre-nap channel would sit there believing it
/// still owns keys the firmware has taken back.
fn session_action(running: &KeyboardTarget, want: &Wanted) -> SessionAction {
    if want.target() == Some(running) {
        SessionAction::Keep
    } else {
        SessionAction::Restart
    }
}

/// Snapshot the keyboard session target, naming the reason when there is
/// none.
///
/// The two idle reasons are deliberately distinct: "no keyboard is published"
/// and "something else claimed the receiver" look identical from the outside
/// and need entirely different fixes.
fn wanted_this_tick(receiver_access: &ReceiverAccess, spec: &SharedKeyboardSpec) -> Wanted {
    if receiver_access.exclusive_requested() {
        return Wanted::Idle(IdleReason::ExclusiveRequested);
    }
    match spec.read().ok().and_then(|guard| guard.clone()) {
        Some(spec) => Wanted::Session(KeyboardTarget::for_spec(spec)),
        None => Wanted::Idle(IdleReason::NoKeyboard),
    }
}

/// Why the watcher currently has no capture session.
///
/// Tracked across ticks so the reason is logged when it *changes*. At one tick
/// per second, logging every tick would be tens of thousands of lines a day;
/// logging none is how a watcher that cannot arm went unnoticed for hours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IdleReason {
    /// The published spec names no keyboard with a route, so there is nothing
    /// to capture.
    NoKeyboard,
    /// Pairing or a host transition has asked for the receiver, so capture
    /// stands aside until it is done.
    ExclusiveRequested,
    /// A keyboard is wanted and nothing claimed the receiver, but the shared
    /// lease could not be taken this tick.
    LeaseUnavailable,
}

/// Fold this tick's idle reason into the reason already reported, returning
/// the reason to log — `None` while it is unchanged.
///
/// Passing `None` for `current` marks the watcher armed again, so the next
/// idle period is reported even when it has the same reason as the last one.
fn idle_transition(
    reported: &mut Option<IdleReason>,
    current: Option<IdleReason>,
) -> Option<IdleReason> {
    if *reported == current {
        return None;
    }
    *reported = current;
    current
}

/// Keep one keyboard capture session alive for the published spec, restarting
/// it when the keyboard or its bound-key set changes, and dispatch incoming
/// presses. Runs for the lifetime of the process.
async fn manage(
    spec: SharedKeyboardSpec,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<KeyboardInput>();
    let mut current: Option<RunningKeyboardSession> = None;
    let mut reported_idle: Option<IdleReason> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);
    // Sessions report completion tagged with their start epoch, so an
    // unexpected exit of the *current* session re-arms while stale completions
    // are ignored — same pacing/starvation reasoning as the gesture watcher.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<HidppSessionId>();

    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                let live_spec = spec.read().ok().and_then(|guard| guard.clone());
                let deliverable = accepts_input(&input.session, current.as_ref())
                    && !receiver_access.exclusive_requested()
                    && current
                        .as_ref()
                        .zip(live_spec.as_ref())
                        .is_some_and(|(running, live)| running.target.matches(live));
                if !deliverable {
                    dispatcher.cancel_hidpp_session(&input.session);
                    debug!(epoch = input.session.epoch(), "input from a stale keyboard session — ignored");
                    continue;
                }
                let Some(live_spec) = live_spec else {
                    continue;
                };
                dispatch_input(&input.session, input.input, &live_spec, &dispatcher);
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release the capture
                // session so run_pairing can own the receiver's HID node.
                let want = wanted_this_tick(&receiver_access, &spec);
                if let Some(running) = current.as_mut() {
                    // Stop a session that no longer matches the spec; sending
                    // on the oneshot lets it restore the diverted controls.
                    // The entry stays tracked — stop sender taken — until its
                    // task reports completion below, and a tracked keyboard
                    // is never re-armed: arming the replacement while the old
                    // task may still be mid-restore could interleave its
                    // divert writes with the restore writes on the same
                    // device, leaving a control un-diverted while the new
                    // session believes it owns it (the gesture manager
                    // documents the same hazard).
                    let keep = session_action(&running.target, &want) == SessionAction::Keep;
                    if !keep && let Some(stop) = running.stop.take() {
                        dispatcher.cancel_hidpp_session(&running.id);
                        let _ = stop.send(());
                    }
                    continue;
                }
                let target = match want {
                    Wanted::Session(target) => target,
                    Wanted::Idle(reason) => {
                        if let Some(reason) = idle_transition(&mut reported_idle, Some(reason)) {
                            info!(?reason, "keyboard capture idle");
                        }
                        continue;
                    }
                };
                let Some(session) = spawn_session(
                    target,
                    &receiver_access,
                    &keyboard_channel,
                    &registry,
                    &tx,
                    &done_tx,
                ) else {
                    if let Some(reason) =
                        idle_transition(&mut reported_idle, Some(IdleReason::LeaseUnavailable))
                    {
                        info!(?reason, "keyboard capture idle");
                    }
                    continue;
                };
                idle_transition(&mut reported_idle, None);
                current = Some(session);
            }
            Some(done_session) = done_rx.recv() => {
                // The session's task has fully exited — restore writes
                // included — so clearing the slot lets the next tick arm a
                // successor, paced by TARGET_POLL; a stale epoch belongs to a
                // session already superseded (see `on_done`).
                if let DoneAction::Remove { unexpected } = on_done(&done_session, current.as_ref()) {
                    dispatcher.cancel_hidpp_session(&done_session);
                    if unexpected {
                        warn!("keyboard capture session ended unexpectedly, re-arming");
                    }
                    current = None;
                }
            }
        }
    }
}

/// Start a capture session for `target`, or `None` when pairing or a host
/// transition holds the receiver — the caller reports that as an idle reason
/// rather than retrying silently.
fn spawn_session(
    target: KeyboardTarget,
    receiver_access: &ReceiverAccess,
    keyboard_channel: &CaptureChannel,
    registry: &ChannelRegistry,
    inputs: &mpsc::UnboundedSender<KeyboardInput>,
    done: &mpsc::UnboundedSender<HidppSessionId>,
) -> Option<RunningKeyboardSession> {
    let receiver_lease = receiver_access.try_acquire_for_session()?;
    let (stop_tx, stop_rx) = oneshot::channel();
    let slot = Arc::clone(keyboard_channel);
    let session_registry = registry.clone();
    let id = HidppSessionId::new(&target.config_key);
    let (sink, mut session_rx) = mpsc::unbounded_channel();
    let forward = inputs.clone();
    let forward_id = id.clone();
    tokio::spawn(async move {
        while let Some(input) = session_rx.recv().await {
            let _ = forward.send(KeyboardInput {
                session: forward_id.clone(),
                input,
            });
        }
    });
    let done = done.clone();
    let done_id = id.clone();
    let route = target.route.clone();
    let wanted = target.wanted.clone();
    tokio::spawn(async move {
        let _receiver_lease = receiver_lease;
        if let Err(e) = run_keyboard_capture_session_with_registry(
            route,
            wanted,
            sink,
            stop_rx,
            slot,
            &session_registry,
        )
        .await
        {
            warn!(error = %e, "keyboard capture session ended with an error");
        }
        let _ = done.send(done_id);
    });
    Some(RunningKeyboardSession {
        id,
        target,
        stop: Some(stop_tx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DpiCycles;
    use crate::runtime::ActionRuntime;

    fn target() -> KeyboardTarget {
        KeyboardTarget {
            rearm_generation: 0,
            config_key: "keyboard-a".to_string(),
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc548,
            },
            wanted: BTreeMap::new(),
        }
    }

    fn session_id(epoch: u64) -> HidppSessionId {
        HidppSessionId::with_epoch("keyboard-a", epoch)
    }

    fn draining_session(epoch: u64) -> RunningKeyboardSession {
        RunningKeyboardSession {
            id: session_id(epoch),
            target: target(),
            stop: None,
        }
    }

    fn live_session(epoch: u64) -> RunningKeyboardSession {
        let (stop, _rx) = oneshot::channel();
        RunningKeyboardSession {
            stop: Some(stop),
            ..draining_session(epoch)
        }
    }

    #[test]
    fn rearms_when_the_current_session_dies() {
        assert_eq!(
            on_done(&session_id(7), Some(&live_session(7))),
            DoneAction::Remove { unexpected: true }
        );
    }

    #[test]
    fn settles_a_draining_session_quietly() {
        assert_eq!(
            on_done(&session_id(7), Some(&draining_session(7))),
            DoneAction::Remove { unexpected: false }
        );
    }

    #[test]
    fn ignores_stale_and_untracked_completions() {
        assert_eq!(
            on_done(&session_id(6), Some(&live_session(7))),
            DoneAction::Ignore
        );
        assert_eq!(on_done(&session_id(7), None), DoneAction::Ignore);
    }

    #[test]
    fn accepts_inputs_only_from_the_current_live_session() {
        assert!(accepts_input(&session_id(7), Some(&live_session(7))));
        assert!(
            !accepts_input(&session_id(6), Some(&live_session(7))),
            "a superseded session's queued input is stale"
        );
        assert!(
            !accepts_input(&session_id(7), Some(&draining_session(7))),
            "a draining session's queued input must not enter the replacement lifecycle"
        );
        assert!(!accepts_input(&session_id(7), None));
    }

    /// Build a spec the way the orchestrator does, so the tests below travel
    /// the real `KeyboardSpec` -> `KeyboardTarget` path instead of asserting
    /// on a hand-built target.
    fn spec_with(keys: &[u8], generation: u64) -> KeyboardSpec {
        let wanted: BTreeMap<u8, ButtonId> = keys
            .iter()
            .map(|&position| (position, ButtonId::KeyFunction(position)))
            .collect();
        KeyboardSpec {
            config_key: "keyboard-a".to_string(),
            route: target().route,
            bindings: wanted
                .values()
                .map(|&button| {
                    (
                        button,
                        Binding::Single(openlogi_core::binding::Action::VolumeUp),
                    )
                })
                .collect(),
            wanted,
            rearm_generation: generation,
        }
    }

    fn target_with(keys: &[u8], generation: u64) -> KeyboardTarget {
        KeyboardTarget::for_spec(spec_with(keys, generation))
    }

    fn wanted_with(keys: &[u8], generation: u64) -> Wanted {
        Wanted::Session(target_with(keys, generation))
    }

    #[test]
    fn an_unchanged_target_keeps_its_session() {
        let running = target_with(&[4, 5], 7);

        assert_eq!(
            session_action(&running, &wanted_with(&[4, 5], 7)),
            SessionAction::Keep
        );
    }

    #[test]
    fn a_changed_bound_key_set_restarts_the_session() {
        let running = target_with(&[4, 5], 7);

        assert_eq!(
            session_action(&running, &wanted_with(&[4, 5, 13], 7)),
            SessionAction::Restart
        );
    }

    #[test]
    fn a_power_cycled_keyboard_restarts_its_session() {
        // Same keyboard, same route, same bound keys — only the capture
        // re-arm generation moved, which is all a reconnect or a system wake
        // changes. The firmware dropped the diversions across that power
        // cycle, so keeping the session would leave every bound key dead
        // while the watcher believed it was capturing them. This is the
        // overnight failure: the gesture watcher has always honoured this
        // generation and the keyboard watcher did not.
        let running = target_with(&[4, 5], 7);

        assert_eq!(
            session_action(&running, &wanted_with(&[4, 5], 8)),
            SessionAction::Restart
        );
    }

    #[test]
    fn losing_the_keyboard_stops_the_session() {
        let running = target_with(&[4, 5], 7);

        assert_eq!(
            session_action(&running, &Wanted::Idle(IdleReason::NoKeyboard)),
            SessionAction::Restart
        );
    }

    #[test]
    fn an_exclusive_request_stops_the_session_so_pairing_can_own_the_receiver() {
        let running = target_with(&[4, 5], 7);

        assert_eq!(
            session_action(&running, &Wanted::Idle(IdleReason::ExclusiveRequested)),
            SessionAction::Restart
        );
    }

    #[test]
    fn an_unchanged_idle_reason_is_reported_once() {
        let mut reported = None;

        assert_eq!(
            idle_transition(&mut reported, Some(IdleReason::NoKeyboard)),
            Some(IdleReason::NoKeyboard)
        );
        // A one-second tick would otherwise write tens of thousands of
        // identical lines a day.
        assert_eq!(
            idle_transition(&mut reported, Some(IdleReason::NoKeyboard)),
            None
        );
        assert_eq!(
            idle_transition(&mut reported, Some(IdleReason::NoKeyboard)),
            None
        );
    }

    #[test]
    fn a_changed_idle_reason_is_reported_again() {
        let mut reported = Some(IdleReason::NoKeyboard);

        assert_eq!(
            idle_transition(&mut reported, Some(IdleReason::ExclusiveRequested)),
            Some(IdleReason::ExclusiveRequested),
            "a wanted keyboard that cannot take the receiver is the case that \
             used to be silent"
        );
    }

    #[test]
    fn idleness_after_a_session_is_reported_even_with_the_same_reason() {
        let mut reported = Some(IdleReason::NoKeyboard);

        // Arming clears the reported reason...
        assert_eq!(idle_transition(&mut reported, None), None);
        // ...so losing the keyboard again says so, instead of being swallowed
        // as "already reported" — the shape of the overnight silence.
        assert_eq!(
            idle_transition(&mut reported, Some(IdleReason::NoKeyboard)),
            Some(IdleReason::NoKeyboard)
        );
    }

    #[test]
    fn captured_insert_runs_its_binding_through_the_action_runtime() {
        let (action_ring, mut actions) = tokio::sync::mpsc::unbounded_channel();
        let mut runtime = ActionRuntime::new(
            Arc::new(RwLock::new(DpiCycles::default())),
            Arc::new(RwLock::new(None)),
            ChannelRegistry::default(),
            ReceiverAccess::default(),
            action_ring,
        )
        .expect("action runtime");
        let spec = KeyboardSpec {
            rearm_generation: 0,
            config_key: "keyboard-a".to_string(),
            route: target().route,
            wanted: BTreeMap::from([(13, ButtonId::KeyFunction(13))]),
            bindings: BTreeMap::from([(
                ButtonId::KeyFunction(13),
                Binding::Single(openlogi_core::binding::Action::ShowActionsRing),
            )]),
        };
        let session = session_id(7);

        dispatch_input(
            &session,
            CapturedInput::ButtonDown(ButtonId::KeyFunction(13)),
            &spec,
            &runtime.dispatcher(),
        );

        assert_eq!(
            actions.blocking_recv(),
            Some(Some("keyboard-a".to_string()))
        );
        dispatch_input(
            &session,
            CapturedInput::ButtonUp(ButtonId::KeyFunction(13)),
            &spec,
            &runtime.dispatcher(),
        );
        runtime.shutdown();
    }
}
