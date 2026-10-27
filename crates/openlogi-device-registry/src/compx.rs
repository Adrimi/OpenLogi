//! Compx 2.4 GHz mouse raw-HID identities.
//!
//! Compx (VID `0x3554`) supplies the receiver and mouse firmware behind many
//! third-party gaming mice, sold under the vendor's "Control HUB" software.
//! The receiver exposes a vendor collection on interface 1
//! (`0xff05/0x0000`) carrying 17-byte reports `0x08`; the protocol itself
//! lives in `openlogi-device`'s `compx` module.
//!
//! The product IDs are the ones the Zaopin Wireless Mouse driver
//! (v1.0.2.0, `usersetting/Config.ini` and `1.ini`) lists for this family.
//! `0xf524` is verified against hardware; the rest come from that table.

/// Compx's USB vendor ID.
pub const COMPX_VENDOR_ID: u16 = 0x3554;
/// Stable driver-family identifier carried by standalone Compx inventory.
pub const COMPX_DRIVER_ID: &str = "compx";
/// Usage page of the Compx vendor collection.
pub const COMPX_USAGE_PAGE: u16 = 0xff05;
/// Usage ID of the Compx vendor collection.
pub const COMPX_USAGE_ID: u16 = 0x0000;

/// How the mouse reaches the host through this USB identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompxLink {
    /// A 2.4 GHz receiver relaying to a paired mouse.
    Receiver,
    /// The mouse itself on a USB cable.
    Wired,
}

/// A known Compx raw-HID identity and its static product metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompxDescriptor {
    /// USB product ID.
    pub product_id: u16,
    /// How the mouse is reached through this product ID.
    pub link: CompxLink,
    /// Marketed product name.
    pub name: &'static str,
}

impl CompxDescriptor {
    const fn zaopin(product_id: u16, link: CompxLink) -> Self {
        Self {
            product_id,
            link,
            name: "Zaopin Wireless Mouse",
        }
    }
}

/// All Compx raw-HID identities OpenLogi recognizes.
pub const COMPX_DEVICES: &[CompxDescriptor] = &[
    CompxDescriptor::zaopin(0xf524, CompxLink::Receiver),
    CompxDescriptor::zaopin(0xf527, CompxLink::Receiver),
    CompxDescriptor::zaopin(0xf5d5, CompxLink::Receiver),
    CompxDescriptor::zaopin(0xf517, CompxLink::Receiver),
    CompxDescriptor::zaopin(0xf548, CompxLink::Receiver),
    CompxDescriptor::zaopin(0xf526, CompxLink::Wired),
    CompxDescriptor::zaopin(0xf511, CompxLink::Wired),
    CompxDescriptor::zaopin(0xf515, CompxLink::Wired),
    CompxDescriptor::zaopin(0xf549, CompxLink::Wired),
];

/// Finds a Compx descriptor by its complete raw-HID identity.
#[must_use]
pub fn find_compx(
    vendor_id: u16,
    product_id: u16,
    usage_page: u16,
    usage_id: u16,
) -> Option<&'static CompxDescriptor> {
    if (vendor_id, usage_page, usage_id) != (COMPX_VENDOR_ID, COMPX_USAGE_PAGE, COMPX_USAGE_ID) {
        return None;
    }
    COMPX_DEVICES
        .iter()
        .find(|device| device.product_id == product_id)
}

#[cfg(test)]
mod tests {
    use super::{COMPX_DEVICES, COMPX_DRIVER_ID, CompxLink, find_compx};

    #[test]
    fn product_ids_are_unique() {
        for (index, device) in COMPX_DEVICES.iter().enumerate() {
            assert!(
                COMPX_DEVICES[..index]
                    .iter()
                    .all(|other| other.product_id != device.product_id),
                "duplicate Compx product id {:04x}",
                device.product_id
            );
        }
    }

    #[test]
    fn vendor_collection_of_the_verified_receiver_matches() {
        let receiver = find_compx(0x3554, 0xf524, 0xff05, 0x0000).expect("F524 receiver");

        assert_eq!(receiver.link, CompxLink::Receiver);
        assert_eq!(receiver.name, "Zaopin Wireless Mouse");
    }

    #[test]
    fn sibling_collections_and_other_vendors_do_not_match() {
        assert!(find_compx(0x3554, 0xf524, 0x0001, 0x0002).is_none());
        assert!(find_compx(0x3554, 0xf524, 0xff03, 0x0000).is_none());
        assert!(find_compx(0x046d, 0xf524, 0xff05, 0x0000).is_none());
        assert!(find_compx(0x3554, 0x1234, 0xff05, 0x0000).is_none());
    }

    #[test]
    fn driver_id_is_stable() {
        assert_eq!(COMPX_DRIVER_ID, "compx");
    }
}
