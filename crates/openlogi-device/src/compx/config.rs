//! The Compx mouse's configuration flash.
//!
//! Every field carries its own check byte computed with [`checksum`] over
//! the field: a single-byte setting is stored as `(value, 0x55 − value)`, a
//! table entry as its data bytes followed by one check byte. The vendor
//! parser (`MouseConfigParser` in `HIDUsb.dll`) fixes the addresses below.
//!
//! | address | field |
//! |---------|-------|
//! | `0x00` | polling rate ([`ReportRate`]) |
//! | `0x02` | number of DPI stages |
//! | `0x04` | active DPI stage, zero-based |
//! | `0x0c` | eight DPI stages, `[x, y, high bits, check]` |
//! | `0x2c` | eight stage colours, `[r, g, b, check]` |
//! | `0x60` | sixteen buttons, `[kind, a, b, check]` ([`ButtonAction`]) |
//! | `0xa9` | button debounce, milliseconds |
//!
//! The remaining fields (`0x06`–`0x0a`, `0x4c`–`0x5f`, `0xab`–`0xed`) hold
//! lighting, sensor and power settings whose order the parser does not
//! name. [`FlashImage`] keeps them byte for byte, and no writer here touches
//! them.
//!
//! DPI is stored in 50-DPI steps minus one. Values above 12 800 DPI spill
//! into the high-bits byte, whose bit layout is not recovered, so such a
//! stage reads as an error rather than as a guess.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use thiserror::Error;

use super::{Opcode, PAYLOAD_LEN, Request, RequestError, Response, checksum};

/// Bytes of configuration flash the vendor driver reads.
pub const FLASH_LEN: usize = 0xf0;
/// DPI stages the flash table holds.
pub const MAX_DPI_STAGES: usize = 8;
/// Programmable buttons the flash table holds.
pub const BUTTON_COUNT: usize = 16;

const REPORT_RATE: u16 = 0x00;
const STAGE_COUNT: u16 = 0x02;
const ACTIVE_STAGE: u16 = 0x04;
const DPI_TABLE: u16 = 0x0c;
const COLOR_TABLE: u16 = 0x2c;
const BUTTON_TABLE: u16 = 0x60;
const DEBOUNCE: u16 = 0xa9;
const ENTRY_LEN: u16 = 4;
const DPI_STEP: u16 = 50;
const MAX_LOW_BYTE_DPI: u16 = 256 * DPI_STEP;
const CHUNK_COUNT: usize = FLASH_LEN.div_ceil(PAYLOAD_LEN);

/// A configuration-flash image that does not hold a valid configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// A field whose check byte does not match its data.
    #[error("flash field at {0:#04x} fails its check byte")]
    Corrupt(u16),
    /// A field holding a value the vendor driver does not define.
    #[error("flash field at {address:#04x} holds unsupported value {value:#04x}")]
    Unsupported {
        /// Field address.
        address: u16,
        /// Offending byte.
        value: u8,
    },
    /// Not every chunk of the image has been read.
    #[error("flash image is missing chunk {0}")]
    Incomplete(usize),
    /// A reply that is not a flash read of this image.
    #[error("reply is not a flash read inside the configuration image")]
    UnexpectedReply,
}

/// A configuration change the encoder refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConfigChangeError {
    /// A DPI stage list empty or longer than [`MAX_DPI_STAGES`].
    #[error("{0} DPI stages; the mouse holds 1 to {MAX_DPI_STAGES}")]
    StageCount(usize),
    /// An active stage outside the configured list.
    #[error("stage {stage} is outside the {count} configured stages")]
    StageOutOfRange {
        /// Requested zero-based stage.
        stage: u8,
        /// Configured stage count.
        count: u8,
    },
    /// A button index at or beyond [`BUTTON_COUNT`].
    #[error("button {0} is outside the {BUTTON_COUNT} programmable buttons")]
    ButtonOutOfRange(usize),
    /// The framed request could not be built.
    #[error(transparent)]
    Request(#[from] RequestError),
}

/// The configuration flash as read from the mouse, chunk by chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashImage {
    bytes: [u8; FLASH_LEN],
    filled: [bool; CHUNK_COUNT],
}

impl Default for FlashImage {
    fn default() -> Self {
        Self {
            bytes: [0; FLASH_LEN],
            filled: [false; CHUNK_COUNT],
        }
    }
}

impl FlashImage {
    /// The read requests that cover the whole image, in address order.
    pub fn read_requests() -> impl Iterator<Item = Request> {
        (0..CHUNK_COUNT).filter_map(|chunk| {
            let (address, length) = chunk_span(chunk);
            // Every chunk is non-empty and at most PAYLOAD_LEN long.
            Request::read_flash(address, length).ok()
        })
    }

    /// Store one flash-read reply.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnexpectedReply`] for a reply that is not a flash read
    /// of one of [`Self::read_requests`]'s chunks.
    pub fn accept(&mut self, response: &Response) -> Result<(), ConfigError> {
        let start = usize::from(response.address());
        let chunk = start / PAYLOAD_LEN;
        if response.opcode() != Opcode::ReadFlash
            || start % PAYLOAD_LEN != 0
            || chunk >= CHUNK_COUNT
        {
            return Err(ConfigError::UnexpectedReply);
        }
        let (_, length) = chunk_span(chunk);
        let length = usize::from(length);
        let payload = response.payload();
        if payload.len() < length {
            return Err(ConfigError::UnexpectedReply);
        }
        self.bytes[start..start + length].copy_from_slice(&payload[..length]);
        self.filled[chunk] = true;
        Ok(())
    }

    /// The raw image, for diagnostics.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; FLASH_LEN] {
        &self.bytes
    }

    fn complete(&self) -> Result<&[u8; FLASH_LEN], ConfigError> {
        match self.filled.iter().position(|filled| !filled) {
            Some(chunk) => Err(ConfigError::Incomplete(chunk)),
            None => Ok(&self.bytes),
        }
    }
}

fn chunk_span(chunk: usize) -> (u16, u8) {
    let start = chunk * PAYLOAD_LEN;
    let length = (FLASH_LEN - start).min(PAYLOAD_LEN);
    // Both are bounded by FLASH_LEN (0xf0) and PAYLOAD_LEN (10).
    (
        u16::try_from(start).unwrap_or(u16::MAX),
        u8::try_from(length).unwrap_or(0),
    )
}

fn field(bytes: &[u8; FLASH_LEN], address: u16, len: u16) -> Result<&[u8], ConfigError> {
    let start = usize::from(address);
    let entry = &bytes[start..=start + usize::from(len)];
    let (data, check) = entry.split_at(usize::from(len));
    if check[0] == checksum(data) {
        Ok(data)
    } else {
        Err(ConfigError::Corrupt(address))
    }
}

fn single(bytes: &[u8; FLASH_LEN], address: u16) -> Result<u8, ConfigError> {
    Ok(field(bytes, address, 1)?[0])
}

fn with_check<const N: usize, const M: usize>(data: [u8; N]) -> [u8; M] {
    const { assert!(M == N + 1) };
    let mut entry = [0; M];
    entry[..N].copy_from_slice(&data);
    entry[N] = checksum(&data);
    entry
}

/// Polling rate, as the flash and the vendor app encode it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ReportRate {
    /// 125 Hz.
    Hz125 = 0x08,
    /// 250 Hz.
    Hz250 = 0x04,
    /// 500 Hz.
    Hz500 = 0x02,
    /// 1000 Hz.
    Hz1000 = 0x01,
    /// 2000 Hz.
    Hz2000 = 0x10,
    /// 4000 Hz.
    Hz4000 = 0x20,
    /// 8000 Hz.
    Hz8000 = 0x40,
}

impl ReportRate {
    /// The rate in hertz.
    #[must_use]
    pub fn hertz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
            Self::Hz2000 => 2000,
            Self::Hz4000 => 4000,
            Self::Hz8000 => 8000,
        }
    }
}

/// A DPI value the flash can store without its unrecovered high bits:
/// a multiple of 50 from 50 to 12 800.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dpi(u16);

impl Dpi {
    /// Validate a DPI value.
    #[must_use]
    pub fn new(dpi: u16) -> Option<Self> {
        ((DPI_STEP..=MAX_LOW_BYTE_DPI).contains(&dpi) && dpi.is_multiple_of(DPI_STEP))
            .then_some(Self(dpi))
    }

    /// The value in DPI.
    #[must_use]
    pub fn get(self) -> u16 {
        self.0
    }

    fn from_step(step: u8) -> Self {
        Self((u16::from(step) + 1) * DPI_STEP)
    }

    fn step(self) -> u8 {
        // `new` bounds the value to 50..=12_800, so the step is 0..=255.
        u8::try_from(self.0 / DPI_STEP - 1).unwrap_or(u8::MAX)
    }
}

/// An RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// One DPI stage: per-axis resolution and the LED colour that marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpiStage {
    /// Horizontal resolution.
    pub x: Dpi,
    /// Vertical resolution.
    pub y: Dpi,
    /// LED colour shown while the stage is active.
    pub color: Rgb,
}

/// A physical mouse button a binding can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum MouseButton {
    /// Left click.
    Left = 0x01,
    /// Right click.
    Right = 0x02,
    /// Wheel click.
    Middle = 0x04,
    /// Back.
    Back = 0x08,
    /// Forward.
    Forward = 0x10,
}

/// What a DPI button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum DpiAction {
    /// Step through the stages, wrapping around.
    Cycle = 0x01,
    /// Next stage.
    Up = 0x02,
    /// Previous stage.
    Down = 0x03,
}

/// A scroll direction a button can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scroll {
    /// Wheel tilt left.
    Left,
    /// Wheel tilt right.
    Right,
    /// One notch up.
    Up,
    /// One notch down.
    Down,
}

/// What one programmable button does, as stored in the button table.
///
/// The kinds come from the vendor app's key catalogue. Fire, shortcut and
/// macro bindings carry parameter bytes whose layout the catalogue does not
/// spell out; they round-trip verbatim. A kind the catalogue does not list
/// reads as [`Self::Unrecognized`] so a stored binding is never rewritten by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    /// The button does nothing.
    Disabled,
    /// Emits a mouse button.
    Mouse(MouseButton),
    /// Changes DPI stage.
    Dpi(DpiAction),
    /// Emits a scroll step.
    Scroll(Scroll),
    /// Repeats a click while held.
    Fire([u8; 2]),
    /// Sends a keyboard shortcut.
    Shortcut([u8; 2]),
    /// Plays a stored macro.
    Macro([u8; 2]),
    /// Cycles the polling rate.
    ReportRateCycle,
    /// A kind outside the vendor catalogue, kept byte for byte.
    Unrecognized {
        /// Kind byte.
        kind: u8,
        /// Parameter bytes.
        parameters: [u8; 2],
    },
}

const KIND_DISABLED: u8 = 0x00;
const KIND_MOUSE: u8 = 0x01;
const KIND_DPI: u8 = 0x02;
const KIND_HORIZONTAL: u8 = 0x03;
const KIND_FIRE: u8 = 0x04;
const KIND_SHORTCUT: u8 = 0x05;
const KIND_MACRO: u8 = 0x06;
const KIND_REPORT_RATE: u8 = 0x07;
const KIND_VERTICAL: u8 = 0x0b;

impl ButtonAction {
    fn decode(address: u16, [kind, a, b]: [u8; 3]) -> Result<Self, ConfigError> {
        let unsupported = |value| ConfigError::Unsupported { address, value };
        Ok(match kind {
            KIND_DISABLED => Self::Disabled,
            KIND_MOUSE => Self::Mouse(MouseButton::try_from(a).map_err(|_| unsupported(a))?),
            KIND_DPI => Self::Dpi(DpiAction::try_from(a).map_err(|_| unsupported(a))?),
            KIND_HORIZONTAL => Self::Scroll(match a {
                0x01 => Scroll::Left,
                0x02 => Scroll::Right,
                other => return Err(unsupported(other)),
            }),
            KIND_VERTICAL => Self::Scroll(match a {
                0x01 => Scroll::Up,
                0x02 => Scroll::Down,
                other => return Err(unsupported(other)),
            }),
            KIND_FIRE => Self::Fire([a, b]),
            KIND_SHORTCUT => Self::Shortcut([a, b]),
            KIND_MACRO => Self::Macro([a, b]),
            KIND_REPORT_RATE => Self::ReportRateCycle,
            kind => Self::Unrecognized {
                kind,
                parameters: [a, b],
            },
        })
    }

    fn encode(self) -> [u8; 3] {
        match self {
            Self::Disabled => [KIND_DISABLED, 0, 0],
            Self::Mouse(button) => [KIND_MOUSE, button.into(), 0],
            Self::Dpi(action) => [KIND_DPI, action.into(), 0],
            Self::Scroll(Scroll::Left) => [KIND_HORIZONTAL, 0x01, 0],
            Self::Scroll(Scroll::Right) => [KIND_HORIZONTAL, 0x02, 0],
            Self::Scroll(Scroll::Up) => [KIND_VERTICAL, 0x01, 0],
            Self::Scroll(Scroll::Down) => [KIND_VERTICAL, 0x02, 0],
            Self::Fire([a, b]) => [KIND_FIRE, a, b],
            Self::Shortcut([a, b]) => [KIND_SHORTCUT, a, b],
            Self::Macro([a, b]) => [KIND_MACRO, a, b],
            Self::ReportRateCycle => [KIND_REPORT_RATE, 0, 0],
            Self::Unrecognized { kind, parameters } => [kind, parameters[0], parameters[1]],
        }
    }
}

/// The mouse settings decoded from a complete [`FlashImage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseConfig {
    /// Polling rate.
    pub report_rate: ReportRate,
    /// Configured DPI stages, at least one.
    pub stages: Vec<DpiStage>,
    /// Zero-based index into [`Self::stages`].
    pub active_stage: u8,
    /// The sixteen button bindings, in flash order.
    pub buttons: [ButtonAction; BUTTON_COUNT],
    /// Button debounce in milliseconds.
    pub debounce_ms: u8,
}

impl MouseConfig {
    /// Decode the settings from a complete image.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when a chunk is missing, a field fails its check byte,
    /// or a field holds a value outside the vendor's definitions.
    pub fn parse(image: &FlashImage) -> Result<Self, ConfigError> {
        let bytes = image.complete()?;

        let rate = single(bytes, REPORT_RATE)?;
        let report_rate = ReportRate::try_from(rate).map_err(|_| ConfigError::Unsupported {
            address: REPORT_RATE,
            value: rate,
        })?;

        let count = single(bytes, STAGE_COUNT)?;
        if count == 0 || usize::from(count) > MAX_DPI_STAGES {
            return Err(ConfigError::Unsupported {
                address: STAGE_COUNT,
                value: count,
            });
        }
        let active_stage = single(bytes, ACTIVE_STAGE)?;
        if active_stage >= count {
            return Err(ConfigError::Unsupported {
                address: ACTIVE_STAGE,
                value: active_stage,
            });
        }
        let stages = (0..u16::from(count))
            .map(|index| stage(bytes, index))
            .collect::<Result<_, _>>()?;

        let mut buttons = [ButtonAction::Disabled; BUTTON_COUNT];
        for (index, slot) in (0u16..).zip(&mut buttons) {
            let address = BUTTON_TABLE + index * ENTRY_LEN;
            let data = field(bytes, address, 3)?;
            *slot = ButtonAction::decode(address, [data[0], data[1], data[2]])?;
        }

        Ok(Self {
            report_rate,
            stages,
            active_stage,
            buttons,
            debounce_ms: single(bytes, DEBOUNCE)?,
        })
    }

    /// The active stage.
    #[must_use]
    pub fn current_stage(&self) -> &DpiStage {
        // `parse` and `set_active_stage` keep the index inside `stages`.
        &self.stages[usize::from(self.active_stage).min(self.stages.len() - 1)]
    }
}

fn stage(bytes: &[u8; FLASH_LEN], index: u16) -> Result<DpiStage, ConfigError> {
    let dpi_address = DPI_TABLE + index * ENTRY_LEN;
    let dpi = field(bytes, dpi_address, 3)?;
    if dpi[2] != 0 {
        return Err(ConfigError::Unsupported {
            address: dpi_address,
            value: dpi[2],
        });
    }
    let color_address = COLOR_TABLE + index * ENTRY_LEN;
    let color = field(bytes, color_address, 3)?;
    Ok(DpiStage {
        x: Dpi::from_step(dpi[0]),
        y: Dpi::from_step(dpi[1]),
        color: Rgb {
            r: color[0],
            g: color[1],
            b: color[2],
        },
    })
}

/// Flash write setting the polling rate.
#[must_use]
pub fn set_report_rate(rate: ReportRate) -> Request {
    write_single(REPORT_RATE, rate.into())
}

/// Flash write setting the button debounce.
#[must_use]
pub fn set_debounce(milliseconds: u8) -> Request {
    write_single(DEBOUNCE, milliseconds)
}

/// Flash write selecting the active stage of `config`'s stage list.
///
/// # Errors
///
/// [`ConfigChangeError::StageOutOfRange`] beyond the configured stages.
pub fn set_active_stage(config: &MouseConfig, stage: u8) -> Result<Request, ConfigChangeError> {
    let count = u8::try_from(config.stages.len()).unwrap_or(u8::MAX);
    if stage >= count {
        return Err(ConfigChangeError::StageOutOfRange { stage, count });
    }
    Ok(write_single(ACTIVE_STAGE, stage))
}

/// Flash writes replacing the DPI stage list with `stages`, activating
/// stage `active`.
///
/// # Errors
///
/// [`ConfigChangeError`] for an empty or oversized list or an active stage
/// outside it.
pub fn set_stages(stages: &[DpiStage], active: u8) -> Result<Vec<Request>, ConfigChangeError> {
    if stages.is_empty() || stages.len() > MAX_DPI_STAGES {
        return Err(ConfigChangeError::StageCount(stages.len()));
    }
    let count =
        u8::try_from(stages.len()).map_err(|_| ConfigChangeError::StageCount(stages.len()))?;
    if active >= count {
        return Err(ConfigChangeError::StageOutOfRange {
            stage: active,
            count,
        });
    }
    let mut requests = vec![
        write_single(STAGE_COUNT, count),
        write_single(ACTIVE_STAGE, active),
    ];
    for (index, stage) in (0u16..).zip(stages) {
        let dpi: [u8; 4] = with_check([stage.x.step(), stage.y.step(), 0]);
        let color: [u8; 4] = with_check([stage.color.r, stage.color.g, stage.color.b]);
        requests.push(Request::write_flash(DPI_TABLE + index * ENTRY_LEN, &dpi)?);
        requests.push(Request::write_flash(
            COLOR_TABLE + index * ENTRY_LEN,
            &color,
        )?);
    }
    Ok(requests)
}

/// Flash write binding button `index` to `action`.
///
/// # Errors
///
/// [`ConfigChangeError::ButtonOutOfRange`] at or beyond [`BUTTON_COUNT`].
pub fn set_button(index: usize, action: ButtonAction) -> Result<Request, ConfigChangeError> {
    let slot = u16::try_from(index)
        .ok()
        .filter(|_| index < BUTTON_COUNT)
        .ok_or(ConfigChangeError::ButtonOutOfRange(index))?;
    let entry: [u8; 4] = with_check(action.encode());
    Ok(Request::write_flash(
        BUTTON_TABLE + slot * ENTRY_LEN,
        &entry,
    )?)
}

fn write_single(address: u16, value: u8) -> Request {
    let entry: [u8; 2] = with_check([value]);
    // Two bytes always fit one report.
    match Request::write_flash(address, &entry) {
        Ok(request) => request,
        Err(_) => unreachable!("a two-byte flash write always fits"),
    }
}

#[cfg(test)]
mod tests;
