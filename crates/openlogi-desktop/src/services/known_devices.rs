//! Third-party devices OpenLogi recognises by the name the OS reports, so the
//! gallery shows the real model and a product photo instead of a generic label.

/// A recognised non-Logitech device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownDevice {
    /// User-facing model name.
    pub name: &'static str,
    /// Embedded asset path of the product photo, served by `AppAssets`.
    pub image: &'static str,
}

/// Asset path of the Zaopin Z2 photo.
pub const ZAOPIN_Z2_IMAGE: &str = "devices/zaopin-z2.png";

/// Match the OS-reported device name against the known third-party devices.
pub fn lookup(os_name: &str) -> Option<KnownDevice> {
    os_name
        .to_ascii_lowercase()
        .contains("zaopin")
        .then_some(KnownDevice {
            name: "Zaopin Z2",
            image: ZAOPIN_Z2_IMAGE,
        })
}

#[cfg(test)]
mod tests {
    use super::lookup;

    #[test]
    fn zaopin_mouse_resolves_to_z2() {
        let device = lookup("Zaopin Wireless Mouse").expect("zaopin is known");
        assert_eq!(device.name, "Zaopin Z2");
    }

    #[test]
    fn unrelated_names_are_not_matched() {
        assert_eq!(lookup("MX Master 3S"), None);
        assert_eq!(lookup("Mouse"), None);
    }
}
