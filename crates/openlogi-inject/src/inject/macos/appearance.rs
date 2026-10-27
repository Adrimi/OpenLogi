use std::ffi::c_void;

use super::app_services::sky_light_symbol;

/// Flip the system appearance. A no-op with a warning when the SPI is
/// missing, the way every other private-SPI helper here degrades.
pub(super) fn toggle_appearance() {
    let Some(api) = appearance_api() else {
        tracing::warn!("SkyLight appearance SPI unavailable — appearance left unchanged");
        return;
    };

    // SAFETY: resolved SkyLight symbols called with the signatures
    // declared below; neither takes a pointer.
    let dark = unsafe { (api.get_theme)() };
    // SAFETY: as above.
    unsafe { (api.set_theme)(!dark) };
    tracing::info!(dark = !dark, "system appearance toggled");
}

#[derive(Clone, Copy)]
struct AppearanceApi {
    get_theme: SlsGetAppearanceThemeFn,
    set_theme: SlsSetAppearanceThemeFn,
}

type SlsGetAppearanceThemeFn = unsafe extern "C" fn() -> bool;
type SlsSetAppearanceThemeFn = unsafe extern "C" fn(bool);

fn appearance_api() -> Option<AppearanceApi> {
    let get_theme = sky_light_symbol(c"SLSGetAppearanceThemeLegacy")?;
    let set_theme = sky_light_symbol(c"SLSSetAppearanceThemeLegacy")?;

    // SAFETY: the symbols, when present, have the private SPI signatures
    // declared above.
    Some(unsafe {
        AppearanceApi {
            get_theme: std::mem::transmute::<*mut c_void, SlsGetAppearanceThemeFn>(get_theme),
            set_theme: std::mem::transmute::<*mut c_void, SlsSetAppearanceThemeFn>(set_theme),
        }
    })
}
