//! The Compx 2.4 GHz mouse protocol, sans I/O.
//!
//! Compx receivers and mice (VID `0x3554`, identities in
//! [`openlogi_device_registry::compx`]) speak no HID++. Everything here was
//! recovered from the vendor's `HIDUsb.dll` (Zaopin Wireless Mouse driver
//! v1.0.2.0) and checked against a `3554:f524` receiver.
//!
//! # Framing
//!
//! Requests and responses are 17-byte reports with id `0x08`, exchanged on
//! the `0xff05` vendor collection of USB interface 1:
//!
//! | byte | meaning |
//! |------|---------|
//! | 0 | report id, always `0x08` |
//! | 1 | [`Opcode`], echoed by the reply |
//! | 2 | status in a reply, `0` on success; `0` in a request |
//! | 3–4 | flash address, big-endian (flash commands only) |
//! | 5 | payload length |
//! | 6–15 | payload |
//! | 16 | checksum: `0x55 − Σ bytes[0..16]` (mod 256) |
//!
//! The vendor driver writes the request, waits for an input report whose
//! opcode matches and whose checksum holds, and retries otherwise. A mouse
//! behind a receiver answers nothing until the host has sent
//! [`Request::pc_driver`] with [`PcDriver::Attached`]; the vendor driver
//! sends it on start and [`PcDriver::Detached`] on exit.
//!
//! Settings live in the mouse's configuration flash, read and written ten
//! bytes at a time; [`config`] owns that layout.
//!
//! # What this module will not build
//!
//! The DLL also carries factory provisioning commands the vendor's own app
//! never imports — rewriting the receiver's VID/PID (`0x0b`) or USB
//! descriptor strings (`0x0c`), the CID/MID pairing identity (`0xf0`), the
//! mouse serial (`0x1f`), and the bootloader and MTK modes (`0x0d`, `0x11`).
//! A wrong value there can leave the receiver unrecognizable to the OS, so
//! they are listed in [`Opcode`] for decoding only and have no builder.

pub mod config;
mod link;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use thiserror::Error;

pub use self::link::{
    KnownMouse, is_compx_route, read_dpi_info, read_report_rate_info, refresh_standalone, set_dpi,
    set_report_rate,
};

/// Report id of every Compx request and response.
pub const REPORT_ID: u8 = 0x08;
/// Length of every Compx report, report id included.
pub const REPORT_LEN: usize = 17;
/// Payload bytes one report carries.
pub const PAYLOAD_LEN: usize = 10;

const STATUS_OFFSET: usize = 2;
const ADDRESS_OFFSET: usize = 3;
const LENGTH_OFFSET: usize = 5;
const PAYLOAD_OFFSET: usize = 6;
const CHECKSUM_OFFSET: usize = 16;
const CHECKSUM_SEED: u8 = 0x55;

/// The Compx checksum: `0x55` minus the byte sum, mod 256.
///
/// The same rule frames reports and guards every configuration-flash field.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(CHECKSUM_SEED, |sum, byte| sum.wrapping_sub(*byte))
}

/// Command byte of a Compx report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
#[non_exhaustive]
pub enum Opcode {
    /// Announce or withdraw the host driver.
    PcDriver = 0x02,
    /// Whether the receiver currently reaches a mouse.
    Online = 0x03,
    /// Mouse battery charge and voltage.
    Battery = 0x04,
    /// Put the receiver into pairing mode.
    EnterPairing = 0x05,
    /// Progress of a pairing started with [`Self::EnterPairing`].
    PairingStatus = 0x06,
    /// Write configuration flash.
    WriteFlash = 0x07,
    /// Read configuration flash.
    ReadFlash = 0x08,
    /// Restore the mouse's factory settings.
    RestoreDefaults = 0x09,
    /// Factory only: rewrite the receiver's VID/PID.
    SetVidPid = 0x0b,
    /// Factory only: rewrite the receiver's USB descriptor strings.
    SetDescriptorString = 0x0c,
    /// Factory only: reboot the receiver into its USB bootloader.
    EnterUpdateMode = 0x0d,
    /// Read the active onboard profile.
    ReadProfile = 0x0e,
    /// Select the active onboard profile.
    SelectProfile = 0x0f,
    /// Factory only: enter the radio test (MTK) mode.
    EnterMtkMode = 0x11,
    /// Receiver firmware version.
    ReceiverVersion = 0x12,
    /// Set the lighting of a 4K receiver.
    SetReceiverRgb = 0x14,
    /// Read the lighting of a 4K receiver.
    ReceiverRgb = 0x15,
    /// Enable or disable long-range radio mode.
    SetLongRange = 0x16,
    /// Whether long-range radio mode is enabled.
    LongRange = 0x17,
    /// Set the receiver's light-bar mode.
    SetReceiverBar = 0x18,
    /// Read the receiver's light-bar mode.
    ReceiverBar = 0x19,
    /// Bind the receiver's identity into the paired mouse.
    BindReceiver = 0x1a,
    /// Factory only: rewrite the mouse serial number.
    SetSerialNumber = 0x1f,
    /// Mouse serial number.
    SerialNumber = 0x20,
    /// Radio signal strength between mouse and receiver.
    Rssi = 0x2b,
    /// Set what the receiver's indicator LED shows.
    SetIndicatorLed = 0x2c,
    /// Read what the receiver's indicator LED shows.
    IndicatorLed = 0x2d,
    /// Start the sensor's surface calibration.
    StartCalibration = 0x2e,
    /// Mouse firmware version.
    MouseVersion = 0xb3,
    /// Factory only: rewrite the CID/MID pairing identity.
    WriteCidMid = 0xf0,
    /// CID/MID pairing identity.
    ReadCidMid = 0xf1,
}

/// Whether the host driver is attached, for [`Request::pc_driver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive)]
#[repr(u8)]
pub enum PcDriver {
    /// No host driver; the mouse stops answering configuration requests.
    Detached = 0x00,
    /// A host driver is present and may configure the mouse.
    Attached = 0x01,
}

/// A request rejected before it was framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RequestError {
    /// More payload than one report carries.
    #[error("payload of {0} bytes exceeds the {PAYLOAD_LEN}-byte report payload")]
    PayloadTooLong(usize),
    /// A zero-length flash transfer.
    #[error("a flash transfer needs at least one byte")]
    EmptyFlashTransfer,
}

/// One framed Compx request, checksum included.
///
/// Only the builders below construct one, so every request on the wire has
/// a known opcode, a payload that fits, and a valid checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    opcode: Opcode,
    bytes: [u8; REPORT_LEN],
}

impl Request {
    fn framed(opcode: Opcode, address: u16, payload: &[u8]) -> Result<Self, RequestError> {
        let length =
            u8::try_from(payload.len()).map_err(|_| RequestError::PayloadTooLong(payload.len()))?;
        if payload.len() > PAYLOAD_LEN {
            return Err(RequestError::PayloadTooLong(payload.len()));
        }
        let mut bytes = [0; REPORT_LEN];
        bytes[0] = REPORT_ID;
        bytes[1] = opcode.into();
        bytes[ADDRESS_OFFSET..LENGTH_OFFSET].copy_from_slice(&address.to_be_bytes());
        bytes[LENGTH_OFFSET] = length;
        bytes[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
        bytes[CHECKSUM_OFFSET] = checksum(&bytes[..CHECKSUM_OFFSET]);
        Ok(Self { opcode, bytes })
    }

    fn bare(opcode: Opcode) -> Self {
        Self::fixed(opcode, &[])
    }

    fn fixed<const N: usize>(opcode: Opcode, payload: &[u8; N]) -> Self {
        const { assert!(N <= PAYLOAD_LEN) };
        let mut bytes = [0; REPORT_LEN];
        bytes[0] = REPORT_ID;
        bytes[1] = opcode.into();
        // `N <= PAYLOAD_LEN` is checked at compile time above.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "N is bounded by PAYLOAD_LEN at compile time"
        )]
        {
            bytes[LENGTH_OFFSET] = N as u8;
        }
        bytes[PAYLOAD_OFFSET..PAYLOAD_OFFSET + N].copy_from_slice(payload);
        bytes[CHECKSUM_OFFSET] = checksum(&bytes[..CHECKSUM_OFFSET]);
        Self { opcode, bytes }
    }

    /// Announce or withdraw the host driver.
    #[must_use]
    pub fn pc_driver(state: PcDriver) -> Self {
        Self::fixed(Opcode::PcDriver, &[state.into()])
    }

    /// Ask whether the receiver currently reaches a mouse.
    #[must_use]
    pub fn online() -> Self {
        Self::bare(Opcode::Online)
    }

    /// Read the mouse battery; decode the reply with [`Battery::decode`].
    #[must_use]
    pub fn battery() -> Self {
        Self::bare(Opcode::Battery)
    }

    /// Put the receiver into pairing mode for the given customer id.
    ///
    /// The vendor driver declares two payload bytes yet places the id in the
    /// third; this mirrors that layout byte for byte.
    #[must_use]
    pub fn enter_pairing(customer_id: u8) -> Self {
        let mut request = Self::fixed(Opcode::EnterPairing, &[0, 0]);
        request.bytes[PAYLOAD_OFFSET + 2] = customer_id;
        request.bytes[CHECKSUM_OFFSET] = checksum(&request.bytes[..CHECKSUM_OFFSET]);
        request
    }

    /// Poll the progress of [`Self::enter_pairing`].
    #[must_use]
    pub fn pairing_status() -> Self {
        Self::bare(Opcode::PairingStatus)
    }

    /// Write `data` to configuration flash at `address`.
    ///
    /// # Errors
    ///
    /// [`RequestError`] when `data` is empty or longer than [`PAYLOAD_LEN`].
    pub fn write_flash(address: u16, data: &[u8]) -> Result<Self, RequestError> {
        if data.is_empty() {
            return Err(RequestError::EmptyFlashTransfer);
        }
        Self::framed(Opcode::WriteFlash, address, data)
    }

    /// Read `length` bytes of configuration flash at `address`.
    ///
    /// # Errors
    ///
    /// [`RequestError`] when `length` is zero or above [`PAYLOAD_LEN`].
    pub fn read_flash(address: u16, length: u8) -> Result<Self, RequestError> {
        match usize::from(length) {
            0 => Err(RequestError::EmptyFlashTransfer),
            len if len > PAYLOAD_LEN => Err(RequestError::PayloadTooLong(len)),
            _ => {
                let mut request = Self::framed(Opcode::ReadFlash, address, &[])?;
                request.bytes[LENGTH_OFFSET] = length;
                request.bytes[CHECKSUM_OFFSET] = checksum(&request.bytes[..CHECKSUM_OFFSET]);
                Ok(request)
            }
        }
    }

    /// Restore the mouse's factory settings.
    #[must_use]
    pub fn restore_defaults() -> Self {
        Self::bare(Opcode::RestoreDefaults)
    }

    /// Read the active onboard profile.
    #[must_use]
    pub fn read_profile() -> Self {
        Self::bare(Opcode::ReadProfile)
    }

    /// Select onboard profile `index`.
    #[must_use]
    pub fn select_profile(index: u8) -> Self {
        Self::fixed(Opcode::SelectProfile, &[index])
    }

    /// Read the receiver firmware version; see [`FirmwareVersion::decode`].
    #[must_use]
    pub fn receiver_version() -> Self {
        Self::bare(Opcode::ReceiverVersion)
    }

    /// Read the mouse firmware version; see [`FirmwareVersion::decode`].
    #[must_use]
    pub fn mouse_version() -> Self {
        Self::bare(Opcode::MouseVersion)
    }

    /// Set a 4K receiver's lighting. Only the mode in the first byte is
    /// decoded; the remaining bytes are forwarded as the vendor app builds
    /// them.
    ///
    /// # Errors
    ///
    /// [`RequestError::PayloadTooLong`] beyond [`PAYLOAD_LEN`] bytes.
    pub fn set_receiver_rgb(payload: &[u8]) -> Result<Self, RequestError> {
        Self::framed(Opcode::SetReceiverRgb, 0, payload)
    }

    /// Read a 4K receiver's lighting.
    #[must_use]
    pub fn receiver_rgb() -> Self {
        Self::bare(Opcode::ReceiverRgb)
    }

    /// Enable or disable long-range radio mode.
    #[must_use]
    pub fn set_long_range(mode: LongRange) -> Self {
        Self::fixed(Opcode::SetLongRange, &[mode.into()])
    }

    /// Read long-range radio mode; see [`LongRange::decode`].
    #[must_use]
    pub fn long_range() -> Self {
        Self::bare(Opcode::LongRange)
    }

    /// Set the receiver's light-bar mode.
    #[must_use]
    pub fn set_receiver_bar(bar: ReceiverBar) -> Self {
        Self::fixed(Opcode::SetReceiverBar, &bar.0)
    }

    /// Read the receiver's light-bar mode; see [`ReceiverBar::decode`].
    #[must_use]
    pub fn receiver_bar() -> Self {
        Self::bare(Opcode::ReceiverBar)
    }

    /// Bind the receiver identity `dongle_id` into the paired mouse.
    ///
    /// The id rides in the last two payload bytes, little-endian, as the
    /// vendor driver stores it.
    #[must_use]
    pub fn bind_receiver(dongle_id: u16) -> Self {
        let mut payload = [0; PAYLOAD_LEN];
        payload[8..].copy_from_slice(&dongle_id.to_le_bytes());
        let mut request = Self::fixed(Opcode::BindReceiver, &payload);
        request.bytes[LENGTH_OFFSET] = 0;
        request.bytes[CHECKSUM_OFFSET] = checksum(&request.bytes[..CHECKSUM_OFFSET]);
        request
    }

    /// Read the mouse serial number.
    #[must_use]
    pub fn serial_number() -> Self {
        Self::bare(Opcode::SerialNumber)
    }

    /// Read the radio signal strength; see [`Response::rssi`].
    #[must_use]
    pub fn rssi() -> Self {
        Self::bare(Opcode::Rssi)
    }

    /// Set what the receiver's indicator LED shows.
    #[must_use]
    pub fn set_indicator_led(led: IndicatorLed) -> Self {
        Self::fixed(Opcode::SetIndicatorLed, &led.to_bytes())
    }

    /// Read the receiver's indicator LED; see [`IndicatorLed::decode`].
    #[must_use]
    pub fn indicator_led() -> Self {
        Self::bare(Opcode::IndicatorLed)
    }

    /// Start the sensor's surface calibration with the vendor parameters.
    ///
    /// # Errors
    ///
    /// [`RequestError::PayloadTooLong`] beyond [`PAYLOAD_LEN`] bytes.
    pub fn start_calibration(parameters: &[u8]) -> Result<Self, RequestError> {
        Self::framed(Opcode::StartCalibration, 0, parameters)
    }

    /// Read the CID/MID pairing identity.
    #[must_use]
    pub fn read_cid_mid() -> Self {
        Self::bare(Opcode::ReadCidMid)
    }

    /// The opcode this request carries; its reply echoes it.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// The report as written to the device, report id first.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; REPORT_LEN] {
        &self.bytes
    }

    /// Whether `response` answers this request.
    ///
    /// Flash replies also echo the address, which distinguishes the chunks of
    /// one multi-report transfer.
    #[must_use]
    pub fn is_answered_by(&self, response: &Response) -> bool {
        response.opcode == self.opcode()
            && (!matches!(self.opcode(), Opcode::ReadFlash | Opcode::WriteFlash)
                || response.address == self.address())
    }

    fn address(&self) -> u16 {
        u16::from_be_bytes([self.bytes[ADDRESS_OFFSET], self.bytes[ADDRESS_OFFSET + 1]])
    }
}

/// A report that is not a well-formed Compx reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResponseError {
    /// Shorter than [`REPORT_LEN`].
    #[error("report of {0} bytes is shorter than a Compx frame")]
    Truncated(usize),
    /// Another report id on the same collection.
    #[error("report id {0:#04x} is not a Compx frame")]
    ForeignReport(u8),
    /// Bytes damaged or not meant for this protocol.
    #[error("checksum {found:#04x} does not match {expected:#04x}")]
    Checksum {
        /// Checksum the report carried.
        found: u8,
        /// Checksum its bytes require.
        expected: u8,
    },
    /// An opcode this driver does not know.
    #[error("unknown Compx opcode {0:#04x}")]
    UnknownOpcode(u8),
    /// The device refused the request.
    #[error("device rejected {opcode:?} with status {status:#04x}")]
    Rejected {
        /// The refused command.
        opcode: Opcode,
        /// Non-zero status byte.
        status: u8,
    },
    /// A reply field outside the values the vendor driver defines.
    #[error("{opcode:?} reply carries unsupported value {value:#04x}")]
    UnsupportedValue {
        /// The command whose reply carried it.
        opcode: Opcode,
        /// The offending byte.
        value: u8,
    },
    /// A typed decoder was handed the reply to another command.
    #[error("expected a {expected:?} reply, got {found:?}")]
    WrongReply {
        /// The decoder's command.
        expected: Opcode,
        /// The reply's command.
        found: Opcode,
    },
}

/// A validated, successful Compx reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    opcode: Opcode,
    address: u16,
    length: u8,
    payload: [u8; PAYLOAD_LEN],
}

impl Response {
    /// Parse and validate one input report.
    ///
    /// # Errors
    ///
    /// [`ResponseError`] for a malformed report or a non-zero status.
    pub fn parse(report: &[u8]) -> Result<Self, ResponseError> {
        let Some(frame) = report.get(..REPORT_LEN) else {
            return Err(ResponseError::Truncated(report.len()));
        };
        if frame[0] != REPORT_ID {
            return Err(ResponseError::ForeignReport(frame[0]));
        }
        let expected = checksum(&frame[..CHECKSUM_OFFSET]);
        if frame[CHECKSUM_OFFSET] != expected {
            return Err(ResponseError::Checksum {
                found: frame[CHECKSUM_OFFSET],
                expected,
            });
        }
        let opcode =
            Opcode::try_from(frame[1]).map_err(|_| ResponseError::UnknownOpcode(frame[1]))?;
        let status = frame[STATUS_OFFSET];
        if status != 0 {
            return Err(ResponseError::Rejected { opcode, status });
        }
        let mut payload = [0; PAYLOAD_LEN];
        payload.copy_from_slice(&frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET]);
        Ok(Self {
            opcode,
            address: u16::from_be_bytes([frame[ADDRESS_OFFSET], frame[ADDRESS_OFFSET + 1]]),
            length: frame[LENGTH_OFFSET],
            payload,
        })
    }

    /// The command this reply answers.
    #[must_use]
    pub fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// The flash address a flash reply echoes.
    #[must_use]
    pub fn address(&self) -> u16 {
        self.address
    }

    /// The payload bytes the reply declares, capped at [`PAYLOAD_LEN`].
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.length).min(PAYLOAD_LEN)]
    }

    fn expect(&self, expected: Opcode) -> Result<&[u8; PAYLOAD_LEN], ResponseError> {
        if self.opcode == expected {
            Ok(&self.payload)
        } else {
            Err(ResponseError::WrongReply {
                expected,
                found: self.opcode,
            })
        }
    }

    /// Signal strength from an [`Opcode::Rssi`] reply, as the raw byte the
    /// receiver reports.
    ///
    /// # Errors
    ///
    /// [`ResponseError::WrongReply`] for another command's reply.
    pub fn rssi(&self) -> Result<u8, ResponseError> {
        Ok(self.expect(Opcode::Rssi)?[0])
    }
}

/// Mouse battery state from an [`Opcode::Battery`] reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Charge, 0–100 %.
    pub percent: u8,
    /// Whether the mouse is on its charging cable.
    pub charging: bool,
    /// Cell voltage in millivolts.
    pub millivolts: u16,
}

impl Battery {
    /// Decode a battery reply.
    ///
    /// # Errors
    ///
    /// [`ResponseError`] for another reply, a charge above 100 %, or a
    /// charging flag other than 0/1.
    pub fn decode(response: &Response) -> Result<Self, ResponseError> {
        let payload = response.expect(Opcode::Battery)?;
        let unsupported = |value| ResponseError::UnsupportedValue {
            opcode: Opcode::Battery,
            value,
        };
        if payload[0] > 100 {
            return Err(unsupported(payload[0]));
        }
        let charging = match payload[1] {
            0 => false,
            1 => true,
            other => return Err(unsupported(other)),
        };
        Ok(Self {
            percent: payload[0],
            charging,
            millivolts: u16::from_be_bytes([payload[2], payload[3]]),
        })
    }
}

/// The mouse serial number from an [`Opcode::SerialNumber`] reply.
///
/// The vendor driver's `UsbFinder_GetMouseSerialNumber` copies the reply's
/// ten payload bytes (report offsets 6–15) to its caller verbatim, so the
/// serial is an opaque ten-byte token; how the factory encodes it inside
/// those bytes is not decoded. A mouse that was never provisioned reads back
/// all zeroes or all `0xff` (blank flash), which is no identity at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SerialNumber([u8; PAYLOAD_LEN]);

impl SerialNumber {
    /// Decode a serial reply; `None` for the blank values that carry no
    /// identity.
    ///
    /// # Errors
    ///
    /// [`ResponseError::WrongReply`] for another command's reply.
    pub fn decode(response: &Response) -> Result<Option<Self>, ResponseError> {
        let payload = *response.expect(Opcode::SerialNumber)?;
        let blank =
            payload.iter().all(|byte| *byte == 0x00) || payload.iter().all(|byte| *byte == 0xff);
        Ok((!blank).then_some(Self(payload)))
    }
}

impl std::fmt::Display for SerialNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// A firmware version, `major.minor` as the vendor app prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FirmwareVersion {
    /// Major version byte.
    pub major: u8,
    /// Minor version byte, printed as two hex digits.
    pub minor: u8,
}

impl FirmwareVersion {
    /// Decode an [`Opcode::ReceiverVersion`] or [`Opcode::MouseVersion`]
    /// reply.
    ///
    /// # Errors
    ///
    /// [`ResponseError::WrongReply`] for any other reply.
    pub fn decode(response: &Response) -> Result<Self, ResponseError> {
        match response.opcode {
            Opcode::ReceiverVersion | Opcode::MouseVersion => Ok(Self {
                major: response.payload[0],
                minor: response.payload[1],
            }),
            found => Err(ResponseError::WrongReply {
                expected: Opcode::ReceiverVersion,
                found,
            }),
        }
    }
}

impl std::fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02x}", self.major, self.minor)
    }
}

/// Long-range radio mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum LongRange {
    /// Normal radio range.
    Off = 0,
    /// Extended range at higher power draw.
    On = 1,
}

impl LongRange {
    /// Decode an [`Opcode::LongRange`] reply.
    ///
    /// # Errors
    ///
    /// [`ResponseError`] for another reply or a mode other than 0/1.
    pub fn decode(response: &Response) -> Result<Self, ResponseError> {
        let value = response.expect(Opcode::LongRange)?[0];
        Self::try_from(value).map_err(|_| ResponseError::UnsupportedValue {
            opcode: Opcode::LongRange,
            value,
        })
    }
}

/// What the receiver's indicator LED reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum IndicatorSource {
    /// LED always off.
    Off = 0,
    /// Polling rate.
    PollingRate = 1,
    /// Mouse battery level.
    Battery = 2,
    /// Radio connection quality.
    ConnectionQuality = 3,
    /// Active DPI stage.
    DpiStage = 4,
}

/// Receiver indicator-LED setting: its source plus two vendor parameter
/// bytes forwarded unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndicatorLed {
    /// What the LED reports.
    pub source: IndicatorSource,
    /// The two bytes the vendor app sends after the source.
    pub parameters: [u8; 2],
}

impl IndicatorLed {
    fn to_bytes(self) -> [u8; 3] {
        [self.source.into(), self.parameters[0], self.parameters[1]]
    }

    /// Decode an [`Opcode::IndicatorLed`] reply.
    ///
    /// # Errors
    ///
    /// [`ResponseError`] for another reply or an unknown source.
    pub fn decode(response: &Response) -> Result<Self, ResponseError> {
        let payload = response.expect(Opcode::IndicatorLed)?;
        let source =
            IndicatorSource::try_from(payload[0]).map_err(|_| ResponseError::UnsupportedValue {
                opcode: Opcode::IndicatorLed,
                value: payload[0],
            })?;
        Ok(Self {
            source,
            parameters: [payload[1], payload[2]],
        })
    }
}

/// Receiver light-bar mode: the six bytes the vendor app builds, carried
/// verbatim because their meaning is not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverBar(pub [u8; 6]);

impl ReceiverBar {
    /// Decode an [`Opcode::ReceiverBar`] reply.
    ///
    /// # Errors
    ///
    /// [`ResponseError::WrongReply`] for another reply.
    pub fn decode(response: &Response) -> Result<Self, ResponseError> {
        let payload = response.expect(Opcode::ReceiverBar)?;
        let mut bar = [0; 6];
        bar.copy_from_slice(&payload[..6]);
        Ok(Self(bar))
    }
}

#[cfg(test)]
mod tests;
