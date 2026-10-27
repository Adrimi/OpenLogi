use std::ffi::{c_uint, c_void};

/// `kAudioObjectSystemObject`.
const SYSTEM_OBJECT: u32 = 1;
/// `kAudioHardwarePropertyDefaultInputDevice` (`'dIn '`).
const DEFAULT_INPUT_DEVICE: u32 = u32::from_be_bytes(*b"dIn ");
/// `kAudioObjectPropertyScopeGlobal` (`'glob'`).
const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
/// `kAudioDevicePropertyScopeInput` (`'inpt'`).
const SCOPE_INPUT: u32 = u32::from_be_bytes(*b"inpt");
/// `kAudioDevicePropertyMute` (`'mute'`).
const PROPERTY_MUTE: u32 = u32::from_be_bytes(*b"mute");
/// `kAudioObjectPropertyElementMain`.
const ELEMENT_MAIN: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const MUTE_ADDRESS: AudioObjectPropertyAddress = AudioObjectPropertyAddress {
    selector: PROPERTY_MUTE,
    scope: SCOPE_INPUT,
    element: ELEMENT_MAIN,
};

/// Flip the default input device's mute state. Devices that do not carry
/// a writable mute property (some USB interfaces) are left alone with a
/// warning rather than silently doing nothing.
pub(super) fn toggle_microphone_mute() {
    let Some(device) = default_input_device() else {
        tracing::warn!("no default audio input device — microphone left unchanged");
        return;
    };
    let Some(muted) = read_mute(device) else {
        tracing::warn!(
            device,
            "input device exposes no readable mute — left unchanged"
        );
        return;
    };
    let wanted = u32::from(muted == 0);
    let address = MUTE_ADDRESS;
    // SAFETY: `device` is a live AudioObjectID, the address is a valid
    // property address, and `wanted` is a `UInt32` of the size declared.
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            &raw const address,
            0,
            std::ptr::null(),
            u32::try_from(size_of::<u32>()).unwrap_or(4),
            (&raw const wanted).cast::<c_void>(),
        )
    };
    if status == 0 {
        tracing::info!(device, muted = wanted == 1, "microphone mute toggled");
    } else {
        tracing::warn!(device, status, "AudioObjectSetPropertyData(mute) failed");
    }
}

fn default_input_device() -> Option<u32> {
    let address = AudioObjectPropertyAddress {
        selector: DEFAULT_INPUT_DEVICE,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut device: u32 = 0;
    let mut size = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: the system object always exists; `device` and `size` are
    // valid out-parameters of the declared size.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &raw const address,
            0,
            std::ptr::null(),
            &raw mut size,
            (&raw mut device).cast::<c_void>(),
        )
    };
    (status == 0 && device != 0).then_some(device)
}

fn read_mute(device: u32) -> Option<u32> {
    let address = MUTE_ADDRESS;
    // SAFETY: `device` is a live AudioObjectID and the address is valid.
    if unsafe { AudioObjectHasProperty(device, &raw const address) } == 0 {
        return None;
    }
    let mut muted: u32 = 0;
    let mut size = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: as above; `muted` and `size` are valid out-parameters.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &raw const address,
            0,
            std::ptr::null(),
            &raw mut size,
            (&raw mut muted).cast::<c_void>(),
        )
    };
    (status == 0).then_some(muted)
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectHasProperty(object: u32, address: *const AudioObjectPropertyAddress) -> c_uint;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectSetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> i32;
}
