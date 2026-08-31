//! Live key capture for one keyboard: divert the bound F-row controls over
//! HID++ `0x1b04` and turn their physical edges into [`CapturedInput`] the agent can
//! dispatch.
//!
//! [`run_keyboard_capture_session`] is the keyboard counterpart of
//! [`crate::session::gesture::run_capture_session`]: one open channel, diversion armed
//! on exactly the controls the caller asks for (an unbound key is never
//! diverted, so it keeps its native firmware function), one message listener,
//! and every diverted control handed back to the firmware on shutdown.
//!
//! Diversion works on the key's *control* — the printed media/shortcut
//! function — so it fires when Fn-lock is off (or via Fn+key when it is on).
//! The plain F1–F12 codes of an Fn-locked row travel the ordinary HID keyboard
//! interface and never reach `0x1b04`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        wireless_device_status::{WirelessDeviceStatusEvent, WirelessDeviceStatusFeature},
    },
    protocol::v20,
};
use openlogi_core::binding::ButtonId;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::capture_restore::{
    ArmedReporting, CaptureStop, ReprogRestore, divert_change, drop_listener_after,
    restore_after_stop, rollback_capture_start, stop_for_current_publication,
    wait_for_channel_change,
};
use super::gesture::{
    CaptureChannel, CaptureSessionFailure, CaptureSessionOutcome, CapturedInput, GestureError,
    PendingCaptureRestore, enumerate_controls,
};
use crate::backend::{BackendError, HidBackend};
use crate::channel::route::{DeviceRoute, open_route_channel};
use crate::{ChannelRegistry, DeviceIoGate, SharedChannel};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};

/// Capture the requested keyboard controls on `route` until `shutdown`
/// resolves, forwarding [`CapturedInput::ButtonDown`] and
/// [`CapturedInput::ButtonUp`] edges to `sink`.
///
/// `wanted` maps physical F-key positions to the [`ButtonId`] they dispatch
/// as. The session resolves each position through the keyboard's live
/// `0x1b04` control table, because control IDs vary between keyboard models.
pub async fn run_keyboard_capture_session(
    backend: &dyn HidBackend,
    route: DeviceRoute,
    wanted: BTreeMap<u8, ButtonId>,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    if !device_io.allows_io() {
        return Err(device_io_suspended().into());
    }
    let chan = open_route_channel(backend, &route)
        .await
        .map_err(GestureError::from)?
        .ok_or(GestureError::DeviceNotFound)?;
    let shared = SharedChannel::new(chan, route.clone());
    run_keyboard_capture_session_on(
        shared,
        wanted,
        sink,
        shutdown,
        channel_slot,
        None,
        device_io,
    )
    .await
}

/// Run keyboard capture on the exact channel currently published by `registry`.
///
/// A registry miss returns [`GestureError::DeviceNotFound`] without falling
/// back to route enumeration/opening; the agent watcher retries after a later
/// inventory publication.
pub async fn run_keyboard_capture_session_with_registry(
    route: DeviceRoute,
    wanted: BTreeMap<u8, ButtonId>,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    registry: &ChannelRegistry,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    let shared = registry
        .lookup(&route)
        .ok_or(GestureError::DeviceNotFound)?;
    run_keyboard_capture_session_on(
        shared,
        wanted,
        sink,
        shutdown,
        channel_slot,
        Some(registry),
        device_io,
    )
    .await
}

async fn run_keyboard_capture_session_on(
    shared: SharedChannel,
    wanted: BTreeMap<u8, ButtonId>,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
    registry: Option<&ChannelRegistry>,
    device_io: DeviceIoGate,
) -> Result<CaptureSessionOutcome, CaptureSessionFailure> {
    if !device_io.allows_io() {
        return Err(device_io_suspended().into());
    }
    let chan = Arc::clone(shared.channel());
    let device_index = shared.device_index();
    let device = Device::new(Arc::clone(&chan), device_index)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(device_index))?;

    let info = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
        .ok_or_else(|| GestureError::Hidpp("keyboard exposes no 0x1b04 reprog controls".into()))?;
    let rc = ReprogControlsV4::new(Arc::clone(&chan), device_index, info.index);
    let controls = enumerate_controls(&rc).await?;
    let mut armed = ArmedKeys {
        controls: rc,
        reporting: Vec::new(),
        diverted: BTreeMap::new(),
    };
    if let Err(error) = arm_keys(&controls, &wanted, &mut armed).await {
        let pending = armed.into_pending(&shared);
        return Err(rollback_capture_start(error, pending, &shared, registry).await);
    }

    // Physical press state per CID. Behind a `Mutex` because the channel's
    // read thread invokes the listener by shared reference.
    let held: Arc<Mutex<BTreeSet<u16>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let feature_index = armed.controls.feature_index();
    let listener = chan.add_msg_listener_guarded({
        let held = Arc::clone(&held);
        let diverted = armed.diverted.clone();
        let sink = sink.clone();
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            let Some(RawControlEvent::DivertedButtons(cids)) =
                reprog_controls::decode_event(&msg, device_index, feature_index)
            else {
                return;
            };
            // Recover the guard even if a prior holder panicked — the critical
            // section is panic-free, so the data is consistent.
            let mut down = held.lock().unwrap_or_else(PoisonError::into_inner);
            emit_button_edges(&mut down, &cids, &diverted, &sink);
        }
    });

    // Wireless keyboards drop their diverted-control state when they
    // power-cycle (idle sleep, power switch, Easy-Switch host change) — the
    // reconnection broadcast on `0x1d4b` is the firmware asking the host to
    // reconfigure. Re-arm the diversion on every broadcast, or the bound keys
    // silently revert to their native functions after the first nap.
    let wireless = device
        .root()
        .get_feature(WirelessDeviceStatusFeature::ID)
        .await
        .ok()
        .flatten()
        .map(|info| WirelessDeviceStatusFeature::new(Arc::clone(&chan), device_index, info.index));

    // Publish this keyboard's open channel so hardware writes (Fn-lock)
    // reuse it instead of opening the same HID node a second time. Cleared
    // on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(shared.clone());
    }

    info!(
        index = device_index,
        keys = armed.diverted.len(),
        wake_rearm = wireless.is_some(),
        "keyboard key capture active"
    );
    let stop = monitor_keyboard_capture(
        KeyboardMonitor {
            armed: &armed,
            device_index,
            registry,
            shared: &shared,
        },
        wireless,
        shutdown,
        device_io,
    )
    .await;

    // The slot is a last-writer-wins cell, so a sibling session may have
    // published its own channel after ours. Clear it only while it still
    // holds *this* session's channel — evicting the sibling's would silently
    // demote its hardware writes to the fresh-open slow path (the gesture
    // session applies the same discipline).
    if let Ok(mut slot) = channel_slot.write()
        && slot
            .as_ref()
            .is_some_and(|shared| Arc::ptr_eq(shared.channel(), &chan))
    {
        *slot = None;
    }
    let pending = armed.into_pending(&shared);
    // Keep accepting edges until firmware restoration is complete. The agent
    // drains this listener's forwarding task before publishing ordered Done,
    // so this session remains the sole owner of every input captured while
    // its controls could still be diverted.
    let outcome = drop_listener_after(
        listener,
        restore_after_stop(stop, pending, &shared, registry),
    )
    .await;
    debug!(index = device_index, "keyboard key capture stopped");
    Ok(outcome)
}

struct KeyboardMonitor<'a> {
    armed: &'a ArmedKeys,
    device_index: u8,
    registry: Option<&'a ChannelRegistry>,
    shared: &'a SharedChannel,
}

async fn monitor_keyboard_capture(
    context: KeyboardMonitor<'_>,
    wireless: Option<WirelessDeviceStatusFeature>,
    shutdown: oneshot::Receiver<()>,
    mut device_io: DeviceIoGate,
) -> CaptureStop {
    let mut wake_events = wireless.as_ref().map(EmittingFeature::listen);
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        if !device_io.allows_io() && !device_io.wait_until_allowed().await {
            return stop_for_current_publication(context.registry, context.shared);
        }
        tokio::select! {
            biased;

            allowed = device_io.changed() => {
                if allowed.is_none() {
                    return stop_for_current_publication(context.registry, context.shared);
                }
            }
            _ = &mut shutdown => {
                return stop_for_current_publication(context.registry, context.shared);
            }
            transition = wait_for_channel_change(context.registry, context.shared) => {
                info!(index = context.device_index, "inventory replaced or removed keyboard capture channel — restarting session");
                return transition;
            }
            event = async {
                match wake_events.as_ref() {
                    Some(events) => events.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let Some(WirelessDeviceStatusEvent::StatusBroadcast(broadcast)) = event else {
                    wake_events = None;
                    continue;
                };
                info!(?broadcast, "keyboard reconnected — re-arming key diversion");
                rearm_keys(context.armed, &device_io).await;
            }
        }
    }
}

/// Diff one full diverted-control snapshot into exactly one edge per physical
/// transition. Unchanged snapshots are deliberately silent.
fn emit_button_edges(
    down: &mut BTreeSet<u16>,
    cids: &[u16],
    diverted: &BTreeMap<u16, ButtonId>,
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    for (&cid, &button) in diverted {
        let now = cids.contains(&cid);
        let was = down.contains(&cid);
        if now && !was {
            let _ = sink.send(CapturedInput::ButtonDown(button));
        } else if !now && was {
            let _ = sink.send(CapturedInput::ButtonUp(button));
        }
        if now {
            down.insert(cid);
        } else {
            down.remove(&cid);
        }
    }
}

struct ArmedKeys {
    controls: ReprogControlsV4,
    reporting: Vec<ArmedReporting>,
    diverted: BTreeMap<u16, ButtonId>,
}

impl ArmedKeys {
    fn into_pending(self, retired: &SharedChannel) -> Option<PendingCaptureRestore> {
        let feature_index = self.controls.feature_index();
        PendingCaptureRestore::new(
            retired,
            ReprogRestore::new(feature_index, self.reporting),
            None,
        )
    }
}

/// Divert every wanted F-row position the keyboard exposes as divertable,
/// adding the resolved CIDs to dispatch state and every possibly-applied
/// write to rollback state. Positions the board does not expose are skipped
/// so support degrades per key.
async fn arm_keys(
    controls: &[reprog_controls::CtrlIdInfo],
    wanted: &BTreeMap<u8, ButtonId>,
    armed: &mut ArmedKeys,
) -> Result<(), GestureError> {
    for (&position, &button) in wanted {
        if let Some(cid) = divertable_cid_at_position(controls, position) {
            let original = armed
                .controls
                .get_cid_reporting(cid)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
            // A transport failure does not prove the firmware rejected the
            // command, so include this CID in rollback before writing.
            armed.reporting.push(ArmedReporting { cid, original });
            armed
                .controls
                .set_cid_reporting_full(cid, divert_change(original, false))
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
            armed.diverted.insert(cid, button);
        } else {
            debug!(
                position,
                "bound key not divertable on this keyboard — left native"
            );
        }
    }
    Ok(())
}

/// The F-row position the agent assigns a keyboard's Insert key — the slot
/// `openlogi-agent-core`'s `function_key_position` maps the macOS `ins`
/// keycode onto, shared with F13 because no board carries both.
///
/// A board that really has an F13 reports a control at this position and
/// resolves through the ordinary lookup below. MX Keys Mini has no F13: its
/// Insert key sits outside the function row, so the firmware reports it at
/// position 0 and it can only be found by control identity.
const INSERT_POSITION: u8 = 13;

/// Control IDs the Insert slot is known to be exposed under, most likely
/// first. On MX Keys Mini the key printed `Insert` *is* the volume-up key —
/// its firmware function is Volume Up, so that is the control `0x1b04`
/// diverts. Full-size boards that expose a genuine Insert control are matched
/// by the other two.
const INSERT_CIDS: [u16; 3] = [
    reprog_controls::control_ids::RE_PROGRAMMABLE_VOLUME_UP.0,
    reprog_controls::control_ids::MULTIPLATFORM_INSERT.0,
    reprog_controls::control_ids::INSERT.0,
];

fn divertable_cid_at_position(
    controls: &[reprog_controls::CtrlIdInfo],
    position: u8,
) -> Option<u16> {
    let divertable = |cid: u16| {
        controls
            .iter()
            .any(|control| control.cid == cid && control.is_divertable())
    };

    if let Some(control) = controls
        .iter()
        .find(|control| control.position == position && control.is_divertable())
    {
        return Some(control.cid);
    }
    if position != INSERT_POSITION {
        return None;
    }
    INSERT_CIDS.into_iter().find(|&cid| divertable(cid))
}

/// Re-issue diversion for every armed control after a device power-cycle.
/// Failures are logged, not propagated — the next reconnection broadcast
/// retries.
async fn rearm_keys(armed: &ArmedKeys, device_io: &DeviceIoGate) {
    // A settling pause: the broadcast arrives the instant the link is back,
    // occasionally before the device accepts feature writes again.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    if !device_io.allows_io() {
        return;
    }
    for &reporting in &armed.reporting {
        if let Err(e) = armed
            .controls
            .set_cid_reporting_full(reporting.cid, divert_change(reporting.original, false))
            .await
        {
            warn!(
                cid = format_args!("{:#06x}", reporting.cid),
                error = ?e,
                "re-divert after wake failed — key stays native until next wake"
            );
        }
    }
}

fn device_io_suspended() -> GestureError {
    GestureError::Hid(BackendError::Backend("host device I/O is suspended".into()))
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;
    use std::time::Duration;

    use super::*;
    use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

    /// The `0x1b04` control table an MX Keys Mini (`046d:b369`) really reports,
    /// captured from the hardware with `openlogi diag controls`. Rows are
    /// `(cid, task_id, flags, position)` in the order the firmware returns
    /// them.
    ///
    /// Two facts about this board drive the whole Insert path, and neither is
    /// guessable from a synthetic table:
    ///
    /// - Easy-Switch (F1-F3) is **not** divertable, so those keys can never
    ///   carry a binding.
    /// - The key printed `Insert` is the Volume Up control, and it sits
    ///   *outside* the function row — position `0`, not `13`. Resolving it by
    ///   position alone finds nothing, which is exactly why a bound Insert
    ///   used to do nothing at all.
    const MX_KEYS_MINI_CONTROLS: [(u16, u16, u16, u8); 16] = [
        (0x00d1, 0x00ae, 0x040a, 1),  // Host Switch channel 1
        (0x00d2, 0x00af, 0x040a, 2),  // Host Switch channel 2
        (0x00d3, 0x00b0, 0x040a, 3),  // Host Switch channel 3
        (0x00e2, 0x00c1, 0x043a, 4),  // Backlight -
        (0x00e3, 0x00c2, 0x043a, 5),  // Backlight +
        (0x0103, 0x00d8, 0x043a, 6),  // Voice Dictation
        (0x0108, 0x00dd, 0x043a, 7),  // Open emoji panel
        (0x010a, 0x00df, 0x043a, 8),  // Snipping tool
        (0x011c, 0x00f1, 0x043a, 9),  // Mute microphone
        (0x00e5, 0x0004, 0x043a, 10), // Play/Pause
        (0x00e7, 0x0003, 0x043a, 11), // Mute
        (0x00e8, 0x0002, 0x043a, 12), // Volume Down
        (0x00e9, 0x0001, 0x0434, 0),  // Volume Up — the key printed `Insert`
        (0x0117, 0x00ec, 0x0434, 0),  // Delete
        (0x00de, 0x0062, 0x0400, 0),  // F Lock
        (0x0034, 0x0062, 0x0000, 0),  // reports no capabilities at all
    ];

    /// Feature index the scripted keyboard answers `0x1b04` on.
    const REPROG_INDEX: u8 = 0x09;
    /// `Re-programmable Volume Up` — the control an MX Keys Mini `Insert`
    /// press actually reports.
    const VOLUME_UP_CID: u16 = 0x00e9;

    fn mx_keys_mini_controls() -> Vec<reprog_controls::CtrlIdInfo> {
        MX_KEYS_MINI_CONTROLS
            .into_iter()
            .map(
                |(cid, task_id, flags, position)| reprog_controls::CtrlIdInfo {
                    cid,
                    task_id,
                    flags,
                    position,
                },
            )
            .collect()
    }

    #[test]
    fn keyboard_snapshots_emit_balanced_edges_without_duplicates() {
        let diverted = BTreeMap::from([
            (0x00d4, ButtonId::KeySearch),
            (0x0103, ButtonId::KeyDictation),
        ]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        emit_button_edges(&mut down, &[0x00d4], &diverted, &sink);
        emit_button_edges(&mut down, &[0x00d4], &diverted, &sink);
        emit_button_edges(&mut down, &[0x00d4, 0x0103], &diverted, &sink);
        emit_button_edges(&mut down, &[0x0103], &diverted, &sink);
        emit_button_edges(&mut down, &[], &diverted, &sink);

        assert_eq!(
            std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                CapturedInput::ButtonDown(ButtonId::KeySearch),
                CapturedInput::ButtonDown(ButtonId::KeyDictation),
                CapturedInput::ButtonUp(ButtonId::KeySearch),
                CapturedInput::ButtonUp(ButtonId::KeyDictation),
            ]
        );
    }

    #[test]
    fn mx_keys_mini_function_row_resolves_each_position_to_its_own_control() {
        let controls = mx_keys_mini_controls();

        for (position, expected) in [
            (4, 0x00e2),
            (5, 0x00e3),
            (6, 0x0103),
            (7, 0x0108),
            (8, 0x010a),
            (9, 0x011c),
            (10, 0x00e5),
            (11, 0x00e7),
            (12, 0x00e8),
        ] {
            assert_eq!(
                divertable_cid_at_position(&controls, position),
                Some(expected),
                "F{position} must divert its own control, not a neighbour's"
            );
        }
    }

    #[test]
    fn mx_keys_mini_easy_switch_keys_cannot_be_bound() {
        let controls = mx_keys_mini_controls();

        for position in 1..=3 {
            assert_eq!(
                divertable_cid_at_position(&controls, position),
                None,
                "F{position} is an Easy-Switch key the firmware refuses to divert, \
                 so the app must not offer it"
            );
        }
    }

    #[test]
    fn mx_keys_mini_insert_resolves_to_volume_up_despite_reporting_position_zero() {
        let controls = mx_keys_mini_controls();

        assert!(
            !controls
                .iter()
                .any(|control| control.position == INSERT_POSITION),
            "the premise of the fallback: this board reports nothing at position 13"
        );
        assert_eq!(
            divertable_cid_at_position(&controls, INSERT_POSITION),
            Some(VOLUME_UP_CID)
        );
    }

    #[test]
    fn insert_never_steals_a_real_f13_control() {
        let mut controls = mx_keys_mini_controls();
        controls.push(reprog_controls::CtrlIdInfo {
            cid: 0x0100,
            task_id: 0,
            flags: 0x043a,
            position: INSERT_POSITION,
        });

        assert_eq!(
            divertable_cid_at_position(&controls, INSERT_POSITION),
            Some(0x0100),
            "a board that really carries F13 must resolve by position"
        );
    }

    #[test]
    fn a_volume_up_report_becomes_an_insert_press_and_release() {
        let diverted = BTreeMap::from([(VOLUME_UP_CID, ButtonId::KeyFunction(INSERT_POSITION))]);
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let mut down = BTreeSet::new();

        emit_button_edges(&mut down, &[VOLUME_UP_CID], &diverted, &sink);
        emit_button_edges(&mut down, &[], &diverted, &sink);

        assert_eq!(
            std::iter::from_fn(|| inputs.try_recv().ok()).collect::<Vec<_>>(),
            vec![
                CapturedInput::ButtonDown(ButtonId::KeyFunction(INSERT_POSITION)),
                CapturedInput::ButtonUp(ButtonId::KeyFunction(INSERT_POSITION)),
            ]
        );
    }

    /// The whole path, over a scripted MX Keys Mini serving the captured table
    /// above: enumerate `0x1b04`, resolve the bound Insert slot, divert the
    /// control it really lives on, and turn that control's press/release
    /// broadcast back into the button the agent dispatches.
    ///
    /// The unit tests above each pin one link. This one is the test that
    /// fails when a bound Insert silently does nothing on the desk.
    #[tokio::test]
    async fn a_bound_insert_is_diverted_and_captured_end_to_end() {
        let (raw, handle) = ScriptedRawHidChannel::with_responder(mx_keys_mini);
        let channel = scripted_channel(raw).await;
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb369,
        };
        let device_index = route.device_index();
        let (sink, mut inputs) = mpsc::unbounded_channel();
        let (stop, shutdown) = oneshot::channel();
        let slot: CaptureChannel = Arc::new(RwLock::new(None));

        let (_io_signal, device_io) = crate::device_io::device_io_channel();
        let session = tokio::spawn(run_keyboard_capture_session_on(
            SharedChannel::new(channel, route.clone()),
            BTreeMap::from([(INSERT_POSITION, ButtonId::KeyFunction(INSERT_POSITION))]),
            sink,
            shutdown,
            Arc::clone(&slot),
            None,
            device_io,
        ));

        // The session publishes its channel only after the event listener is
        // registered, so from here on an emitted report cannot be missed.
        await_capture_active(&slot).await;

        let written = handle.written_reports();
        assert!(
            written
                .iter()
                .any(|report| is_divert_of(report, VOLUME_UP_CID)),
            "Insert must be armed on the Volume Up control; wrote {written:02x?}"
        );

        handle.emit(diverted_buttons_event(device_index, &[VOLUME_UP_CID]));
        handle.emit(diverted_buttons_event(device_index, &[]));

        assert_eq!(
            inputs.recv().await,
            Some(CapturedInput::ButtonDown(ButtonId::KeyFunction(
                INSERT_POSITION
            )))
        );
        assert_eq!(
            inputs.recv().await,
            Some(CapturedInput::ButtonUp(ButtonId::KeyFunction(
                INSERT_POSITION
            )))
        );

        let _ = stop.send(());
        // Firmware restoration is the shared capture-teardown path's concern,
        // not this test's: what matters here is that the bound slot armed the
        // right control and that its broadcast came back as the right button.
        let _outcome = session
            .await
            .expect("the capture task must not panic")
            .expect("the capture session must end cleanly");
    }

    /// Poll until the session publishes its channel, so the test cannot race
    /// the listener registration. Bounded so a regression fails instead of
    /// hanging the suite.
    async fn await_capture_active(slot: &CaptureChannel) {
        for _ in 0..200 {
            if slot.read().is_ok_and(|slot| slot.is_some()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the keyboard capture session never became active");
    }

    /// Whether `report` is the `setCidReporting` write that temporarily
    /// diverts `cid` (`diverted` valid+set, `raw_xy` valid+clear).
    fn is_divert_of(report: &[u8], cid: u16) -> bool {
        let [cid_hi, cid_lo] = cid.to_be_bytes();
        report.len() >= 7
            && report[0] == 0x11
            && report[2] == REPROG_INDEX
            && report[3] >> 4 == 0x03
            && report[4] == cid_hi
            && report[5] == cid_lo
            && report[6] == 0b0010_0011
    }

    /// One unsolicited `divertedButtonsEvent` naming the CIDs held right now;
    /// an empty `held` is the release snapshot.
    fn diverted_buttons_event(device_index: u8, held: &[u16]) -> Vec<u8> {
        let mut report = vec![0u8; 20];
        report[0] = 0x11;
        report[1] = device_index;
        report[2] = REPROG_INDEX;
        // Function 0, software id 0 — the shape that marks a device event
        // rather than a response to a request.
        report[3] = 0x00;
        for (slot, cid) in held.iter().enumerate() {
            report[4 + slot * 2..6 + slot * 2].copy_from_slice(&cid.to_be_bytes());
        }
        report
    }

    /// An MX Keys Mini answering `0x1b04` from the captured control table.
    fn mx_keys_mini(request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let mut payload = [0u8; 16];
        match (request[2], request[3] >> 4) {
            // Root ping issued by `Device::new`.
            (0x00, 0x01) => payload[0] = 4,
            // Root feature lookup. Only `0x1b04` is implemented, which keeps
            // the wireless wake-up re-arm path out of this test.
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == reprog_controls::FEATURE_ID {
                    payload[0] = REPROG_INDEX;
                }
            }
            // getCount.
            (REPROG_INDEX, 0x00) => {
                payload[0] = u8::try_from(MX_KEYS_MINI_CONTROLS.len()).ok()?;
            }
            // getCidInfo — a long response carrying the full 16-byte row.
            (REPROG_INDEX, 0x01) => {
                let &(cid, task_id, flags, position) =
                    MX_KEYS_MINI_CONTROLS.get(usize::from(request[4]))?;
                let [flags_additional, flags_primary] = flags.to_be_bytes();
                payload[0..2].copy_from_slice(&cid.to_be_bytes());
                payload[2..4].copy_from_slice(&task_id.to_be_bytes());
                payload[4] = flags_primary;
                payload[5] = position;
                payload[8] = flags_additional;
                return Some(long_response(request, payload));
            }
            // getCidReporting. Capture reads the control's current state
            // before writing so it can roll the write back; nothing is
            // diverted yet, so every flag is clear.
            (REPROG_INDEX, 0x02) => {
                payload[0..2].copy_from_slice(&request[4..6]);
                return Some(long_response(request, payload));
            }
            // setCidReporting — the firmware echoes the change back.
            (REPROG_INDEX, 0x03) => {
                payload[..3].copy_from_slice(&request[4..7]);
                return Some(long_response(request, payload));
            }
            _ => return None,
        }
        Some(short_response(request, payload))
    }

    fn short_response(request: &[u8], payload: [u8; 16]) -> Vec<u8> {
        let mut response = vec![0u8; 7];
        response[0] = 0x10;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload[..3]);
        response
    }

    fn long_response(request: &[u8], payload: [u8; 16]) -> Vec<u8> {
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        response
    }
}
