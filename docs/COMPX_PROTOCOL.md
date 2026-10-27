# Compx 2.4 GHz mouse protocol

Status: reference for the vendor protocol spoken by Compx-based 2.4 GHz
receivers ("Zaopin Wireless Mouse"). Reads are verified on one real receiver
and mouse; everything else is derived from the vendor driver and marked as such.

Confidence labels used throughout:

- **Verified on hardware** - observed on a real receiver and mouse.
- **From the vendor DLL, not yet hardware-verified** - read from the
  disassembly of the vendor driver or its data files; never sent to hardware.

## Overview

A Compx receiver is a USB dongle (VID `0x3554`) that exposes a keyboard, a
mouse and several vendor HID collections. One of those collections, a
vendor-defined 17-byte report channel, carries a small request/reply protocol
that the receiver either answers itself (version, pairing, indicator LED) or
relays to the paired mouse (battery, configuration flash).

The mouse stores its configuration (DPI stages, polling rate, button map,
debounce, colours) in a 0xF0-byte flash image that the host reads and writes
in chunks of at most 10 bytes. There is no HID++ here: framing, opcodes and
checksums are unrelated to Logitech's protocol.

## Provenance and confidence

The protocol was reverse-engineered from the vendor Windows driver
"Zaopin Wireless Mouse v1.0.2.0 (2026-06-17)" (Inno Setup installer; Qt
application plus `HIDUsb.dll`). The DLL is unstripped and exports about 250
named functions such as `UsbFinder_GetBatteryLevel`, `UsbServer_ReadAllFlashData`
and `MouseConfigParser`, which made static analysis with radare2 practical.
The behaviour was then probed read-only on macOS with hidapi against a real
receiver (PID `0xF524`) and a paired mouse.

The vendor software family is "Control HUB" (the same family as the ATK and
VXE web hubs). The driver's `Config.ini` lists:

| Item | Value |
|---|---|
| Sensor | PAW3395 (stored as base64 `MzM5NQ==`) |
| Mouse MCU | NRF52840 |
| Dongle MCU | CX52650N |
| 4K-dongle MCU | CH32V305 |
| DPI range | 50-26000, step 50 |
| Default DPI grades | 400, 800, 1600, 2400, 3200, 6400 |
| Default colours | red, green, blue, yellow, orange, cyan, magenta, white |
| Programmable keys | 16 |
| Debounce default | 8 ms |
| Battery voltage curve | 3050...4110 mV, 21 points (0-100% in 5% steps) |

A separate "Zaopin Z1" installer targets a different chip (VID `0x25A7`,
PID `0xFA7B`/`0xFA7C`, Areson-style) and is **not** this protocol.

## USB identity

- Vendor ID `0x3554`; manufacturer string "Compx", product string
  "2.4G Wireless Receiver".
- Receiver PIDs: `0xF524` (**verified on hardware**); `0xF527`, `0xF5D5`,
  `0xF517`, `0xF548` (from the vendor config, not yet hardware-verified).
- Wired PIDs: `0xF526`, `0xF511`, `0xF515`, `0xF549` (from the vendor config,
  not yet hardware-verified).
- Control interface: USB interface 1, vendor collection usage page `0xFF05`,
  usage `0x0000`, 17-byte input reports (**verified on hardware**).

Other collections on the `0xF524` receiver (**verified on hardware**):

| Interface | Collections |
|---|---|
| 0 | keyboard (`0x01`/`0x06`) |
| 1 | `0xFF05`/`0x00` (control), `0xFF03`, `0xFF02`/`0x02`, `0xFF04`/`0x02`, consumer (`0x0C`/`0x01`), system (`0x01`/`0x80`) |
| 2 | mouse (`0x01`/`0x02`) |

## Framing

All traffic uses 17-byte reports with report ID `0x08` (**verified on
hardware**):

| Byte | Meaning |
|---|---|
| 0 | `0x08` (report ID) |
| 1 | opcode; echoed in the reply |
| 2 | status in a reply: `0` = OK, non-zero = rejected (`0x01` seen when the wrong flag was used) |
| 3-4 | flash address, big-endian (flash commands; echoed in flash replies) |
| 5 | payload length |
| 6-15 | 10-byte payload |
| 16 | checksum = `(0x55 - sum(bytes[0..16])) mod 256` |

Vendor driver behaviour (from the vendor DLL, not yet hardware-verified): post
an overlapped 17-byte read, write the request, wait (100 ms default), and
accept a reply only if its checksum is valid **and** its opcode equals the
request opcode (flash replies also echo the address). It retries N+1 times.

On macOS, plain hidapi `write()`/`read()` on the `0xFF05` collection works
(**verified on hardware**).

## Handshake

Nothing relayed to the mouse answers until the host announces itself with the
PC-driver status command (**verified on hardware**):

- Attach: opcode `0x02`, length 1, `payload[0] = 0x01`.
- Detach on exit: opcode `0x02`, length 1, `payload[0] = 0x00`.

Reply example for attach: `08 02 00 00 00 01 01 ... 49`.

Bit `0x80` of the length byte selects **keyboard mode only** (the DLL sets it in
`SetKeyboardMode`/`KeyboardStart` and clears it in `SetMouseMode`). Never set it
for a mouse: doing so yielded status `0x01` with empty data (**verified on
hardware**).

## Command reference

Confidence column: **V** = verified on hardware; **D** = from the vendor DLL,
not yet hardware-verified. "Reply" lists payload bytes only.

| Opcode | Name | Request payload | Reply payload | Confidence |
|---|---|---|---|---|
| `0x02` | PC driver status | len 1: `0x01` attached, `0x00` detached | echo of state | V |
| `0x03` | Online | none | observed `08 03 00 00 00 01 00 69 b9 9d ...`; payload meaning not decoded | V (request/reply), meaning open |
| `0x04` | Battery | none | `[0]` percent (`0x14` = 20%), `[1]` charging flag (observed 0), `[2..4]` millivolts big-endian (`0x0E1C` = 3612 mV) | V for percent and millivolts; charging-flag meaning D |
| `0x05` | Enter pairing | len 2: `[0..2] = 0`, then `[2]` customer id (layout per DLL) | status | D |
| `0x06` | Pairing status | none | status | D |
| `0x07` | Write flash | address, len <= 10, data | status | D (not sent to hardware) |
| `0x08` | Read flash | address, len <= 10 | requested bytes | V |
| `0x09` | Restore defaults | none | status | D |
| `0x0E` | Read profile | none | active profile | D |
| `0x0F` | Select profile | len 1: `[0]` index | status | D |
| `0x12` | Receiver firmware version | none | `02 17` = "2.17" | V |
| `0x14` | Set 4K-receiver RGB | `[0]` mode, rest undecoded | status | D |
| `0x15` | Get 4K-receiver RGB | none | `[0]` mode, rest undecoded | D |
| `0x16` | Set long-range mode | `[0]` 0 or 1 | status | D |
| `0x17` | Get long-range mode | none | `[0]` 0 or 1 | D |
| `0x18` | Set receiver light bar | 6 bytes, undecoded | status | D |
| `0x19` | Get receiver light bar | none | 6 bytes, undecoded | D |
| `0x1A` | Bind receiver id into mouse | u16 little-endian at `payload[8..10]` | status | D |
| `0x20` | Read serial | none | 10 payload bytes, copied verbatim by the vendor DLL; opaque token, all-zero or all-0xFF = unprovisioned (D) |
| `0x2B` | RSSI | none | `[0]` signal strength | D |
| `0x2C` | Set receiver indicator LED | 3 bytes: source, 2 params | status | D |
| `0x2D` | Get receiver indicator LED | none | 3 bytes: source, 2 params | D |
| `0x2E` | Start sensor calibration | variable params | status | D |
| `0xB3` | Mouse firmware version | none | version | D |
| `0xF1` | Read CID/MID | none | ids | D |

Indicator LED sources, from the vendor option catalogue (D): 0 off,
1 polling rate, 2 battery, 3 connection quality, 4 DPI level.

Factory-only opcodes (`0x0B`, `0x0C`, `0x0D`, `0x11`, `0x1F`, `0xF0`) are
listed under [Factory commands policy](#factory-commands-policy).

Verified reply examples (checksum is the last byte, ellipsis is zero padding):

```
08 02 00 00 00 01 01 ... 49        driver attach ack
08 04 00 00 00 02 14 00 0e 1c ... 09   battery: 20%, not charging, 3612 mV
08 12 00 00 00 02 02 17 ... 20     receiver version 2.17
```

## Configuration flash

Reading is **verified on hardware**. The layout below comes from the DLL's
`MouseConfigParser`. Its polling-rate pair, stage count, active stage and the
active stage's DPI are **verified on hardware**: OpenLogi's parser reads
1000 Hz and 1400 DPI (stage 3 of 5) from the test mouse, matching the dump
below. The other fields are still **from the vendor DLL, not yet
hardware-verified**. Writes of the polling rate and the active stage's DPI are **verified on hardware**; other writes are untested.

The part OpenLogi reads is the first 0xF0 bytes, as 24 chunks of 10 bytes with
opcode `0x08`. The vendor driver's flash image is larger: its parser
(`ProtocolDataParser`, `0x180007be0`) also covers the shortcut payloads at
`0x100`, the macros at `0x300`, a second DPI record table at `0x1B00` and a few
settings up to `0x1B4B`, and it writes scalar pairs as far as `0xFA` (see
[Regions beyond the first 0xF0 bytes](#regions-beyond-the-first-0xf0-bytes)).

Encoding rules:

- Each scalar is stored as a pair `(value, 0x55 - value)`.
- Each table entry is its data bytes followed by one check byte
  `0x55 - sum(data bytes)`.
- A record whose check byte does not match is ignored by the vendor parser,
  which then keeps its built-in default (listed below). `0xFF 0xFF` (erased
  flash) fails the check, so an erased field reads as "default".

### Map

Confidence labels in this section: **app** = established from the vendor
Windows application (`Zaopin Wireless Mouse.exe`, image base `0x140000000`: the
Qt widget slot that edits the field, the setter it calls, and the global
protocol struct at `0x140324b30` that the setter writes before calling
`ProtocolDataUpdate`); **DLL** = established from `HIDUsb.dll` only
(`MouseConfigParser` `0x1800074a0` reads the field, `ProtocolDataCompareUpdate`
body `0x1800082b0` writes it, no UI found); **dump** = consistent with the
hardware dump below; **V** = verified on hardware. All of these remain
unwritten by OpenLogi except `0x00` and the active stage's DPI entry.

| Address | Content | Encoding | Confidence |
|---|---|---|---|
| `0x00` | polling rate | scalar pair, [codes](#polling-rate-codes) | V |
| `0x02` | DPI stage count, 1-8 | scalar pair (DLL default 6) | V |
| `0x04` | active stage, zero-based, below the count | scalar pair (DLL default 2) | V |
| `0x06` | unknown, default 0 | scalar pair; no vendor UI touches it | unknown |
| `0x08` | click trigger mode | scalar pair used as a bit field, see [below](#click-trigger-mode-0x08) | app (polarity low) |
| `0x0A` | lift-off distance | scalar pair, see [below](#sensor-scalars) | app + dump |
| `0x0C` | eight DPI stages | `[x, y, hi, check]`, see [DPI encoding](#dpi-encoding) | V (hi = 0), app |
| `0x2C` | eight stage colours | `[r, g, b, check]` | V (layout), app |
| `0x4C` | DPI indicator LED effect | scalar pair, see [DPI LED](#dpi-indicator-led-0x4c-0x52) | app + dump |
| `0x4E` | DPI indicator LED brightness | scalar pair | app + dump |
| `0x50` | DPI indicator LED speed | scalar pair | app + dump |
| `0x52` | DPI indicator LED enabled | scalar pair | app + dump |
| `0x54`-`0x5F` | unparsed by the vendor driver | `ff 00 ff 57 00 55 80 d5 03 52 00 55` in the dump | unknown |
| `0x60` | sixteen button entries | `[kind, a, b, check]` | V (layout), app |
| `0xA0` | mouse body light, 6-byte record | `[mode, r, g, b, speed, brightness]` + check at `0xA6`, see [mouse light](#mouse-body-light-0xa0-0xa7) | app + dump |
| `0xA7` | mouse body light enabled | scalar pair (DLL default 1) | app + dump |
| `0xA9` | button debounce, ms | scalar pair (DLL default 8) | V (read), app |
| `0xAB` | motion sync | boolean pair (DLL default 1) | app + dump |
| `0xAD` | idle timeout | scalar pair, units of 10 s (default 6), see [idle timeout](#idle-timeout-0xad-and-0xb7) | app + dump |
| `0xAF` | angle snapping | boolean pair (default 0) | app + dump |
| `0xB1` | ripple control | boolean pair (default 0) | app + dump |
| `0xB3` | turn off body light while moving | boolean pair (default 0) | app (medium) + dump |
| `0xB5` | highest-performance mode | boolean pair (default 0) | app + dump |
| `0xB7` | idle timeout (mirror of `0xAD`) | scalar pair, units of 10 s (default 6) | app + dump |
| `0xB9` | sensor mode | scalar pair: 0 LP, 1 HP (default 0) | app + dump |
| `0xBB` | unknown (dump: `0a 4b`, value 10) | scalar pair; neither parsed nor written by the vendor driver | unknown |
| `0xBD` | mouse angle adjustment | signed degrees -30..30 | app (no hardware value) |
| `0xBF` | angle adjustment marker | pair; the vendor app always writes 1 together with `0xBD` | app (meaning low) |
| `0xC1` | unknown | scalar pair | unknown (DLL only) |
| `0xC3`, `0xC8`, `0xCD`, `0xD2` | unknown | four `[4 data bytes, check]` records | unknown (DLL only) |
| `0xD7`, `0xD9` | unknown | scalar pairs | unknown (DLL only) |
| `0xDB` | unknown | `[3 data bytes, check]` | unknown (DLL only) |
| `0xDF` | unknown | scalar pair | unknown (DLL only) |
| `0xE1` | 20K FPS scanning | boolean pair (default 0) | app |
| `0xE3` | wheel (roller) debounce | scalar pair, ms, 10..100 in steps of 10 | app (medium) |
| `0xE5` | button release debounce | scalar pair, ms (combo index) | app (low) |
| `0xE7`, `0xE9` | unknown | scalar pairs | unknown (DLL only) |
| `0xEB` | unknown | `[3 data bytes, check]` | unknown (DLL only) |
| `0xC0`-`0xEF` | all `0xFF` on the test mouse | | V |

"Unknown (DLL only)" fields are parsed into the driver's struct and written back
by `ProtocolDataCompareUpdate`, but nothing in the vendor application reads or
edits them (no setter, no widget). They may serve other products on the same
firmware family. Do not write them.

### Sensor scalars

All **from the vendor app** unless noted. Each is a scalar pair; "boolean" means
0 or 1 (the DLL parses any non-zero value as true).

| Address | Setting | Values | Evidence |
|---|---|---|---|
| `0x0A` | lift-off distance | 3 = 0.7 mm, 1 = 1 mm, 2 = 2 mm (catalogue `LODOptions`, three-item combo for the 3395) | `SensorLOD` slot `0x140141420` calls setter `0x14013b350` with the combo's code; dump `0x0A = 1` |
| `0xAB` | motion sync | 1 on (DLL default), 0 off | class `MotionSync` slot `0x14013d8c0` -> setter `0x14013b030`; dump 1 |
| `0xAF` | angle snapping ("StraightLine") | 1 on, 0 off (default) | class `StraightLine` slot `0x14013cd40` -> setter `0x14013a380`; dump 0 |
| `0xB1` | ripple control | 1 on, 0 off (default) | class `RippeControl` slot `0x14013c1c0` -> setter `0x14013b190`; dump 0 |
| `0xB5` | highest-performance mode | 1 on, 0 off (default) | `Sensor` slot `0x14001a710` -> setter `0x14013b270`; it also enables the duration combo; dump 0 |
| `0xB9` | sensor mode | 0 = LP, 1 = HP (default 0); the third combo row "Corded" is hidden on wireless links and its write is skipped (`edx == 2`) | `Sensor` slot `0x14001a560` -> setter `0x14013b200`; the read path (`0x14000f8ca`) sets the combo index straight from the byte; dump 1 |
| `0xE1` | 20K FPS scanning | 1 on, 0 off (default) | class `KFPS20` -> setter `0x140139f50`; no dump evidence (`0xE1` is erased) |

The class-to-address mapping was made from each Qt class's `qt_metacast`
string (for example the `MotionSync` metacast follows the slot that calls
`0x14013d8c0`), not from guessed names.

### Click trigger mode (`0x08`)

The scalar at `0x08` is used as a two-bit field by class `FastClick` (the
"quick trigger" page, window title "FastClick", labels "左键"/"右键" = left/right
button and "按下即触发"/"按至底触发" = trigger on press / trigger at bottom):
slot 0 sets bit 0, slot 1 clears bit 0, slot 2 sets bit 1, slot 3 clears bit 1
(`0x1401c86b0`-`0x1401c8710`, each tail-calling `0x14013b9c0`). Bit 0 is the left button and bit 1 the right button. The first radio
button of each pair (the "trigger on press" label by widget order) sets the
bit, so bit set = trigger on press is the likely reading, but the polarity was
not confirmed from a read path. The dump holds 0 (neither). **Confidence:
medium for the bit assignment, low for the polarity.**

### Idle timeout (`0xAD` and `0xB7`)

Two scalar pairs (DLL default 6 each) that the vendor app **always writes with
the same value** (setters `0x14013ae30` and `0x14013b2e0` store both). Unit
is 10 seconds (the load path at `0x140070650` multiplies by 10 to get
seconds). Two UI controls feed them, the same time table each:

| Code | Duration |
|---|---|
| 1 | 10 s |
| 3 | 30 s |
| 6 | 1 min |
| 12 | 2 min |
| 18 | 3 min |
| 30 | 5 min |
| 60 | 10 min |
| 90 | 15 min |
| 180 | 30 min (`StopCloseTime` combo only) |
| 210 | 35 min |
| 240 | 40 min |

The controls are class `StopCloseTime` (slot `0x14014bb00`, shown on the
Advanced page, catalogue "Mouse Sleep Time" / light-off time) and class
`Sensor` combo `PerformanceOpen` (slot `0x14001a610`, the duration of the
highest-performance mode). Which of the two bytes the firmware actually reads
for sleep versus the performance window is **not known**; the vendor app never
writes them apart. The dump holds 6 and 6 (1 minute). To stay compatible,
write both to the same value.

### DPI indicator LED (`0x4C`-`0x52`)

Four scalar pairs, laid out like the driver's `BufferToDPILed` (`0x180007a50`)
record: effect, brightness, speed, enabled. **From the vendor app** (class
`SensorEffect`, setters `0x14013a690`, `0x14013a580`, `0x14013a510`):

| Address | Field | Encoding |
|---|---|---|
| `0x4C` | effect | 1 steady, 2 breathing (catalogue `DPIEffectOptions`, 0 off); DLL default 2 |
| `0x4E` | brightness | 0-255. The slider position maps to a code: 1 -> 16, 2 -> 30, 3 -> 60, 4 -> 90, 5 -> 128, 6 -> 150, 7 -> 180, 8 -> 210, 9 -> 230, 10 -> 255 (position 0 not extracted); DLL default 0x80 |
| `0x50` | speed | small integer from the speed slider (range not extracted); DLL default 3 |
| `0x52` | enabled | 0 = effect off, 1 = on; DLL default 1 |

Selecting "off" in the effect combo writes `0x52 = 0` and leaves `0x4C`
untouched; selecting steady or breathing writes `0x52 = 1` and `0x4C = 1 or 2`.
The dump (`02`, `ff`, `03`, `00`) is the breathing effect remembered but the
LED disabled, at full brightness.

### Mouse body light (`0xA0`-`0xA7`)

The mouse's decorative lighting (catalogue "Display light effects", class
`MouseLight`). **From the vendor app** (setters `0x14013adb0`, `0x14013ad10`,
`0x14013aea0`, `0x14013aca0`; the 30-case metacall at `0x1401deb60` matches the
class's moc slot list in order: case 23 `horizontalLight` (brightness), 24
`horizontalSpeed`, 25 `LightMod`, 26 the checkbox):

| Address | Field | Encoding |
|---|---|---|
| `0xA0` | mode | 0 off, 1 rainbow, 2 single-colour breath, 3 fixed colour, 4 neon, 5 rainbow breath, 6 fixed rainbow (catalogue `LightModeOptions`); DLL default 1 |
| `0xA1`-`0xA3` | colour | r, g, b (used by the colour modes); DLL default 255, 255, 255 |
| `0xA4` | speed | slider value minus 1; DLL default 8 |
| `0xA5` | brightness | slider value minus 1; DLL default 9 |
| `0xA6` | check | `0x55 - sum(0xA0..0xA5)` |
| `0xA7` | enabled | 1 if the mode is non-zero, else 0; DLL default 1 |

Turning the mode to "off" writes `0xA7 = 0` and leaves `0xA0`. The record is
the same 6-byte layout the driver uses for the receiver light bar
(`BufferToLedBar`, `0x180007b00`, opcodes `0x18`/`0x19`). The slider's full
range was not extracted (stored speed/brightness are slider value minus 1, so
0-9 if the sliders run 1-10, which the DLL defaults 8 and 9 and the dump value
9 suggest).

`0xB3` is the checkbox on the same page (labelled `MovingOffLight`, "Turn off
decorative lights when moving"): setter `0x14013b0a0`, forced to 0 when the mode
becomes off. **Medium confidence:** the label assignment comes from the page's
language-update function (`0x1401a8c70`), the widget itself is named
`RippeControl` in that page's UI.

### Mouse angle adjustment (`0xBD`, `0xBF`)

Class `AngleMove` ("Mouse Angle Mode"): a slider from -30 to +30 whose value is
stored as a signed byte at `0xBD`, with `0xBF = 1` written at the same time
(setter `0x14013a310`). Neither is set on the test mouse (both erased), so it
reads as the DLL default, 0. The meaning of `0xBF` and of `0xC1` (parsed but
never written by the app) is not known.

### Debounce fields

`0xA9` (button debounce, ms) is edited by class `DebounceTime`
(slot `0x140148cf0`, setter `0x14013a7b0`, value = combo index). Two more
combos on the buttons page, written by setters `0x14013b930` and
`0x14013a820`:

| Address | Label | Encoding |
|---|---|---|
| `0xE3` | wheel (roller) debounce | ms, 10 to 100 in steps of 10 (`(index + 1) * 10`, items "10".."100"); label `RollerDebounceTime` sits beside this combo |
| `0xE5` | second key debounce | ms, combo index (items "0".."N"); the combo is in the "Key Debounce Time" group next to the "Press"/"Release" labels, so this is probably the release delay with `0xA9` the press delay |

The `0xE5` role is **low confidence**; the `0xE3` role is medium. Neither
address holds data on the test mouse.

### Shortcut and macro payloads

Kind 5 and kind 6 buttons keep their keys outside the button table. The layouts
below are decoded from the driver's serialisers and are **from the vendor DLL**
(`BufferToShortcutKey` `0x18000aaf0`, `ShortcutKeyToBuffer` `0x18000aa10`,
`MacroKeyToBuffer` `0x18000abf0`); no hardware dump covers them yet.

Shortcut record, `0x20` bytes at `0x100 + 0x20 * button`:

```
[n] [t0 k0a k0b] [t1 k1a k1b] ... [tn-1 kn-1a kn-1b] [check]
```

with `2 <= n <= 6` (outside that range the record reads as empty), and
`check = 0x55 - sum(all preceding bytes)` stored at offset `3n + 1`. In each
triple the top two bits of `t` give the event (`10` press, `01` release,
otherwise both), the low nibble is the key type (`2` = HID consumer control as
used by multimedia keys; keyboard keys presumably `1`), and `ka`/`kb` carry the
code (for consumer control, the usage low and high byte).

Macro record, `0x180` bytes at `0x300 + 0x180 * button`:

```
[len] [name: len bytes] ... [0x1F]=event count  [0x20..]= 5-byte events  [check]
event = [phase<<6 | type, a, b, delay_hi, delay_lo]
```

The first byte is the length of a block copied verbatim to offset 1 (at most 30
bytes; probably the macro name). The event count sits at offset `0x1F`, events
start at `0x20`, and the check byte follows the last event:
`check = 0x55 - sum(bytes 0x1F .. end of events)`; the name block is not covered
by the check. The 16-bit delay at the end of an event is stored high byte
first. The meaning of `type`, the units of the delay, and how a kind-6 button's
`a`/`b` select a record were not decoded.

### Regions beyond the first 0xF0 bytes

**From the vendor DLL**, not read by OpenLogi and not covered by the dump:

| Address | Content |
|---|---|
| `0xEF`, `0xF1`, `0xF3` | three scalar pairs, defaults 6, 2, 3 |
| `0xF5`, `0xF7`, `0xF9` | three scalar pairs, defaults 6, 2, 3 |
| `0x100` | 16 shortcut payload records of `0x20` bytes |
| `0x300` | 16 macro records of `0x180` bytes |
| `0x1B00` | eight 6-byte records `[x lo, x hi, y lo, y hi, extra, check]`, one per DPI stage (16-bit mirror of the stage table, filled only for non-3395 sensors) |
| `0x1B30`, `0x1B32` | scalar pairs, defaults 0 (vendor app: class `ModePoint`/DPI sensitivity curves) |
| `0x1B34`-`0x1B47` | five `[u16, u8, check]` records (curve points) |
| `0x1B48`, `0x1B4A` | scalar pairs (vendor app: class `VirtualPoint`, setter `0x14013b890`) |

The six scalars at `0xEF`-`0xF9` belong to class `TouchKeySet` with moderate
confidence (setters `0x14013b3c0`-`0x14013b7f0`; bit `0x80` of the second and
third byte of each triple is an enable flag); the touch-key page is not
meaningful for a plain mouse. `0xEF`+ lies past the 0xF0 window OpenLogi reads,
so a firmware that stores real values there needs a longer read.

### Polling rate codes

From the vendor catalogue (D):

| Code | Rate |
|---|---|
| `0x08` | 125 Hz |
| `0x04` | 250 Hz |
| `0x02` | 500 Hz |
| `0x01` | 1000 Hz |
| `0x10` | 2000 Hz |
| `0x20` | 4000 Hz |
| `0x40` | 8000 Hz |

The code `0x01` for 1000 Hz matches the hardware dump (offset `0x00` = `01 54`).

### DPI encoding

For the PAW3395 (Config.ini `Sensor=3395`) the vendor app encodes a stage as
follows (**from the vendor app**: the DPI slot at `0x140017da0`/`0x140018b70`
computes the code, `0x14013a460` and `0x14013a070` store it; the sensor-model
branch is the `3395`/`3950` case):

```
n   = dpi / 50 - 1              (0 .. 519 for 50 .. 26000 DPI)
x   = n & 0xFF
y   = n & 0xFF
hi  = ((n >> 8) << 2) | ((n >> 8) << 6)    ; X high bits in bits 2-3, Y high bits in bits 6-7
```

Both axes are written together by default, X and Y can also be set
independently (the X setter keeps the Y nibble and vice versa). Bits 0, 1, 4
and 5 of `hi` are not written for this sensor (other sensor families use mask
`0x22` to select a 100-step table; irrelevant for the 3395). Decoding is the
inverse: `dpi = (((hi >> 2) & 3) * 256 + x + 1) * 50` for X and
`(((hi >> 6) & 3) * 256 + y + 1) * 50` for Y. Worked values: 12800 DPI is
`n = 255`, `hi = 0`; 12850 DPI is `n = 256`, `x = 0`, `hi = 0x44`; 26000 DPI is
`n = 519`, `x = 7`, `hi = 0x88`.

Confidence: the `hi = 0` range (50 to 12800 DPI) is consistent with the
hardware dump (`0x0F` = 800 DPI) and with what OpenLogi already reads and
writes; the `hi != 0` range comes from the vendor app only and has never been
sent to a mouse. The vendor DLL additionally keeps a second per-stage record at
`0x1B00 + 6 * stage` (16-bit X, 16-bit Y, one extra byte, check) that it
writes when its struct copy changes; the app only fills it for other sensor
models (setter `0x139fc0`, "3955" branch), so on a 3395 it stays untouched.

DPI entries and colour entries are separate flash records written by separate
packets: `ProtocolDataCompareUpdate` compares each stage's DPI entry
(`0x0C + 4 * stage`), colour entry (`0x2C + 4 * stage`) and `0x1B00` record
against the previous copy and sends one write packet per changed record (the
app's slots also call separate setters, `0x14013a460` for DPI and
`0x14013a5f0` for the colour). Changing a DPI value never rewrites its colour.

### Button kinds

From the vendor catalogue (D):

| Kind | Meaning | `a` |
|---|---|---|
| 0 | disabled | |
| 1 | mouse button | `0x01` left, `0x02` right, `0x04` middle, `0x08` back, `0x10` forward |
| 2 | DPI | 1 cycle, 2 up, 3 down |
| 3 | horizontal scroll | 1 left, 2 right |
| `0x0B` | vertical scroll | 1 up, 2 down |
| 4 | fire button | `a` = interval (10-255), `b` = repeat count (0-3; 0 = repeat while held) |
| 5 | shortcut / combo key, also multimedia | `a` = 0, `b` caller-defined; the keys live in a payload record at `0x100 + 0x20 * button`, see [Shortcut payloads](#shortcut-and-macro-payloads) |
| 6 | macro | payload at `0x300 + 0x180 * button`; `a`/`b` not decoded |
| 7 | polling-rate cycle | `a` = 0, `b` = 0 |
| 8 | lighting hotkey | `a` = 0 toggle all decorative lights, 1 toggle DPI indicator LED, 3 toggle light strip, 4 cycle light-strip effect; `b` = 0 |
| 9 | profile ("configuration") switch | `a` = 0, `b` = 0 as written by the app |
| `0x0A` | DPI lock | `a` = DPI code from the app's DPI encoder (`0x140042980`), `b` = 0 |

Kinds 8, 9 and `0x0A` are **from the vendor app** (the button-function radio
handlers call setter `0x14013a9d0`, and the label lookup at `0x1400569f6`
shows the text for each; the Chinese labels are "开关装饰灯", "配置切换" and
"DPI锁定"). The meaning of `a` for kind 9 and the DPI-code range for kind
`0x0A` were not traced further. Fire-key `a`/`b` are the app's `FireKey` dialog
fields (`DialogFireKeyInterval`, `DialogFireKeyTimes` in the catalogue) and
match the dump (`04 0a 03` = interval 10, 3 repeats).

**Multimedia keys are not a separate kind.** The vendor catalogue lists them
under UI id `1005`, but the app stores them as kind 5 plus a one-key shortcut
payload: the button-label code at `0x140056320` checks `kind == 5`, then the
payload's type nibble `== 2` and compares the next byte with the HID consumer
usage (`0x83` media player, `0xCD` play/pause, `0xB5` next, `0xB6` previous,
`0xB7` stop, `0xE2` mute, `0xE9` volume up, `0xEA` volume down, `0x8A` email,
`0x92` calculator, `0x94` my computer, `0x21`-`0x2A` browser keys; the two
bytes presumably carry the usage low and high byte, so `0x0183` would be `83 01`). The type nibble
`2` is consumer control; keyboard keys presumably use `1` (not confirmed).

The entry on the test mouse, `08 04 00` (button 9), is therefore **not**
multimedia: it is a lighting hotkey, kind 8 with `a = 4` ("cycle light-strip
effect"). This was previously listed as an unconfirmed kind.

### Other vendor catalogues

From `Language/en.json` (D):

| Option | Values |
|---|---|
| lift-off distance | 3 = 0.7 mm, 1 = 1 mm, 2 = 2 mm |
| Sensor mode | 0 LP, 1 HP, 256 Corded |
| Performance / light-off timers | 1 = 10 s, 3 = 30 s, 6 = 1 min, 12 = 2 min, 18 = 3 min, 30 = 5 min, 60 = 10 min, 90 = 15 min |
| DPI LED effect | 0 off, 1 steady, 2 breathing |
| Light modes | 0 off, 1 rainbow, 2 single-colour breath, 3 fixed, 4 neon, 5 rainbow breath, 6 fixed rainbow |
| Dynamic-sensitivity curves | 1 classic, 2 natural, 3 jump, 0 custom |

Manual pairing: hold left + right + middle for 3 seconds until the light flashes.

### Annotated hardware dump

Captured from the test mouse (**verified on hardware**). It holds 5 DPI stages
(800, 1000, 1200, 1400, 1600), active stage 3 (stored zero-based value `03`, i.e.
the 1400 DPI stage), 1000 Hz and debounce 8 ms. The readings of the fields
marked "app" come from the vendor application (see the [Map](#map)); the dump
is consistent with them (defaults for the fields the user never changed,
plausible values for the others) but a dump cannot prove a field's meaning.

```
0000: 01 54 05 50 03 52 00 55 00 55 01 54 0f 0f 00 37
0010: 13 13 00 2f 17 17 00 27 1b 1b 00 1f 1f 1f 00 17
0020: 7f 7f 00 57 7f 7f 00 57 7f 7f 00 57 ff 00 00 56
0030: 00 ff 00 56 ff ff 00 57 00 00 ff 56 ff 80 00 d6
0040: 00 ff ff 57 00 ff ff 57 ff ff ff 58 02 53 ff 56
0050: 03 52 00 55 ff 00 ff 57 00 55 80 d5 03 52 00 55
0060: 01 01 00 53 01 02 00 52 01 04 00 50 01 08 00 4c
0070: 01 10 00 44 02 01 00 52 02 02 00 51 02 03 00 50
0080: 07 00 00 4e 08 04 00 49 04 0a 03 44 00 00 00 55
0090: 00 00 00 55 00 00 00 55 00 00 00 55 00 00 00 55
00a0: 01 ff 00 ff 07 09 46 00 55 08 4d 01 54 06 4f 00
00b0: 55 00 55 00 55 00 55 06 4f 01 54 0a 4b ff ff ff
00c0..00ef: all ff
```

Annotations:

| Offset | Bytes | Reading |
|---|---|---|
| `0x00` | `01 54` | polling rate `0x01` = 1000 Hz, check `0x55 - 0x01 = 0x54` |
| `0x02` | `05 50` | 5 DPI stages |
| `0x04` | `03 52` | active stage 3 |
| `0x06` | `00 55` | unknown scalar, value 0 |
| `0x08` | `00 55` | click trigger bits: both 0 |
| `0x0A` | `01 54` | lift-off distance code 1 = 1 mm |
| `0x0C` | `0f 0f 00 37` | stage 0: 16 * 50 = 800 DPI |
| `0x10` | `13 13 00 2f` | stage 1: 20 * 50 = 1000 DPI |
| `0x14` | `17 17 00 27` | stage 2: 24 * 50 = 1200 DPI |
| `0x18` | `1b 1b 00 1f` | stage 3: 28 * 50 = 1400 DPI |
| `0x1C` | `1f 1f 00 17` | stage 4: 32 * 50 = 1600 DPI |
| `0x20`-`0x2B` | `7f 7f 00 57` x3 | unused DPI entries, 6400 DPI placeholder |
| `0x2C` | `ff 00 00 56` | colour 0: red |
| `0x30` | `00 ff 00 56` | colour 1: green |
| `0x34` | `ff ff 00 57` | colour 2: yellow |
| `0x38` | `00 00 ff 56` | colour 3: blue |
| `0x3C` | `ff 80 00 d6` | colour 4: orange |
| `0x40` | `00 ff ff 57` | colour 5: cyan |
| `0x44` | `00 ff ff 57` | colour 6: cyan (as stored) |
| `0x48` | `ff ff ff 58` | colour 7: white |
| `0x4C` | `02 53` | DPI indicator effect 2 (breathing) |
| `0x4E` | `ff 56` | DPI indicator brightness 255 |
| `0x50` | `03 52` | DPI indicator speed 3 |
| `0x52` | `00 55` | DPI indicator disabled (effect value kept) |
| `0x54`-`0x5F` | `ff 00 ff 57 00 55 80 d5 03 52 00 55` | not parsed by the vendor driver; reads like a second lighting record `[colour 255,0,255][0, 0x80, 3, 0]` |
| `0x60` | `01 01 00 53` | button 0: mouse button, left |
| `0x64` | `01 02 00 52` | button 1: mouse button, right |
| `0x68` | `01 04 00 50` | button 2: mouse button, middle |
| `0x6C` | `01 08 00 4c` | button 3: mouse button, back |
| `0x70` | `01 10 00 44` | button 4: mouse button, forward |
| `0x74` | `02 01 00 52` | button 5: DPI cycle |
| `0x78` | `02 02 00 51` | button 6: DPI up |
| `0x7C` | `02 03 00 50` | button 7: DPI down |
| `0x80` | `07 00 00 4e` | button 8: polling-rate cycle |
| `0x84` | `08 04 00 49` | button 9: lighting hotkey, cycle light-strip effect |
| `0x88` | `04 0a 03 44` | button 10: fire button, interval 10, 3 repeats |
| `0x8C`-`0x9F` | `00 00 00 55` x5 | buttons 11-15: disabled |
| `0xA0` | `01 ff 00 ff 07 09 46` | mouse body light: mode 1 (rainbow), colour 255,0,255, speed 7, brightness 9, check `0x46` |
| `0xA7` | `00 55` | mouse body light disabled (mode value kept) |
| `0xA9` | `08 4d` | debounce 8 ms, check `0x55 - 0x08 = 0x4d` |
| `0xAB` | `01 54` | motion sync on |
| `0xAD` | `06 4f` | idle timeout 6 * 10 s = 1 min |
| `0xAF` | `00 55` | angle snapping off |
| `0xB1` | `00 55` | ripple control off |
| `0xB3` | `00 55` | body light stays on while moving |
| `0xB5` | `00 55` | highest-performance mode off |
| `0xB7` | `06 4f` | idle timeout mirror, 1 min |
| `0xB9` | `01 54` | sensor mode 1 (HP) |
| `0xBB` | `0a 4b` | unknown scalar, value 10 |
| `0xBD`-`0xEF` | `ff` | unused (erased) |

The colour rows and the button rows satisfy the check-byte rule (each row's last byte equals `0x55 - sum(first three)`). That is a consistency check, not proof of the semantic reading of each field.

## Factory commands policy

These opcodes exist in the vendor DLL and are used by the manufacturer's
production tooling. OpenLogi deliberately does not expose them, because a wrong
write can leave the receiver unrecognisable to the OS or unable to talk to the
mouse, with no recovery path for an end user:

| Opcode | Function |
|---|---|
| `0x0B` | set VID/PID |
| `0x0C` | set USB descriptor string |
| `0x0D` | enter USB bootloader / update mode |
| `0x11` | MTK radio test mode |
| `0x1F` | write serial |
| `0xF0` | write CID/MID |

The Rust request builders must not construct these, and probe scripts must not
send them.

## How to probe safely

Read-only procedure, in this order:

1. Open the `0xFF05`/`0x0000` collection on interface 1 (not the keyboard or
   mouse collections).
2. Attach: send opcode `0x02` with `payload[0] = 0x01`. Expect status 0.
3. Read-only queries: `0x12` (receiver version), `0x03` (online), `0x04`
   (battery), `0x08` (flash, in 24 chunks of 10 bytes), plus any other
   read-only opcode from the table. Verify the checksum and that the reply
   opcode matches the request before trusting a reply.
4. Detach: send opcode `0x02` with `payload[0] = 0x00`, also on error paths.

Rules:

- Never set the `0x80` length bit for a mouse.
- Never send restore-defaults, profile-select, calibration, binding or factory
  opcodes while they remain unverified. Flash writes are limited to the two
  OpenLogi performs (see below): one entry at a time, read back afterwards.
- Do not probe from a second process while the OpenLogi agent is running.
  Replies carry no request id, only the echoed opcode, so two clients on one
  receiver can take each other's replies; the agent serializes its own
  sessions, but nothing serializes it against another process.
- Close the device cleanly; the receiver stays attached to the "driver" state
  until detached.

## Open questions

- DPI above 12800 DPI: the high-bits layout (bits 2-3 for X, 6-7 for Y) comes
  from the vendor app only; no mouse has been written with such a value.
- Fields still unknown: `0x06`, `0x54`-`0x5F`, `0xBB`, `0xBF` (role), `0xC1`,
  `0xC3`-`0xDF` except as listed, `0xE7`, `0xE9`, `0xEB`; the polarity of the
  `0x08` click-trigger bits; which of `0xAD`/`0xB7` the firmware reads for what;
  the `a`/`b` bytes of kind-6 (macro) and kind-9 buttons and the DPI code of
  kind-`0x0A` buttons; the slider ranges of the DPI LED speed and mouse light.
  See the [Map](#map) for the evidence behind everything else.
- Payload of opcode `0x03` (online) is not decoded.
- Charging flag (`0x04` `payload[1]`): only the value 0 has been observed.
- Multimedia buttons are kind 5 plus a consumer-control payload at
  `0x100 + 0x20 * button` (from the vendor app/DLL, never written to a mouse);
  the observed entry `08 04 00` is a lighting hotkey (kind 8), not multimedia.
- Write path (`0x07`): **verified on hardware** for the polling rate
  (1000 → 500 → 1000 Hz) and the active stage's DPI (1400 → 1450 → 1400 via
  `openlogi api dpi --set`). The firmware needs no commit or reload step: the
  flash read straight after the write shows the new value, and the mouse
  follows it. Writes to other fields are still untested.
- The vendor DLL writes every scalar setting the same way OpenLogi does — a
  2-byte `(value, check)` pair per call from `CS_ProtocolDataCompareUpdate`
  (`0x02`–`0x0A`, `0xA9`–`0xF9`, the lighting records `0x4C`–`0x52` and
  `0xA0`/`0xA7`) — and, contrary to what an earlier version of this document
  said, it also writes the DPI table: one 4-byte packet per changed stage entry
  (`0x0C + 4 * stage`), one per changed colour entry (`0x2C + 4 * stage`) and
  one 6-byte packet per changed `0x1B00 + 6 * stage` record, plus the button
  table (`0x60 + 4 * button`), shortcut records and macros. Colours are never
  bundled with DPI values. The only thing the app sends through
  `UsbFinderC_ReadWriteDeviceData` is a separate periodic date/time packet
  (command `0x10`), not configuration.
- Polling rates above 1000 Hz: offered only when the mouse already reports
  one, because a 1K receiver cannot yet be told apart from a 4K one.
- Payloads of the 4K-receiver RGB and light-bar commands.


## Implementation status in OpenLogi

### Layers

| Layer | File | What it does |
|---|---|---|
| Identity | [`openlogi-device-registry/src/compx.rs`](../crates/openlogi-device-registry/src/compx.rs) | VID, PIDs, usage page/usage, driver id `compx` |
| Protocol (sans I/O) | [`openlogi-device/src/compx.rs`](../crates/openlogi-device/src/compx.rs) | Framing, checksum, typed opcodes, request builders, reply decoders |
| Configuration flash (sans I/O) | [`openlogi-device/src/compx/config.rs`](../crates/openlogi-device/src/compx/config.rs) | Flash image, parser, change builders |
| Session | [`openlogi-device/src/compx/link.rs`](../crates/openlogi-device/src/compx/link.rs) | Attach → requests → detach over a raw report pair; reply matching, timeout, retries |
| Transport | `HidBackend::open_raw_duplex` / `RawDuplex` in [`openlogi-device/src/backend.rs`](../crates/openlogi-device/src/backend.rs), implemented by `NativeRawDuplex` in [`openlogi-hid/src/transport/native.rs`](../crates/openlogi-hid/src/transport/native.rs) | Output reports out, input reports back, on the `0xFF05` collection |
| Discovery | [`openlogi-device/src/inventory/standalone.rs`](../crates/openlogi-device/src/inventory/standalone.rs) | Lists the receiver as a standalone mouse |
| Agent | `HardwareContext` / `Orchestrator` in `openlogi-agent-core`, RPC handlers in `openlogi-agent/src/server.rs` | Routes a Compx `RawHid` route to the driver instead of HID++ |

### Session behaviour

- Every operation is one session: open the collection, attach (`0x02`,
  `0x01`), run its requests, detach (`0x02`, `0x00`) — the detach runs on
  error paths too.
- Sessions are serialized process-wide; replies are matched by echoed opcode
  (and address for flash replies), and reports for other ids are skipped.
- Each attempt waits 250 ms for its reply, three attempts per request. A mouse
  that never answers (asleep, off, out of range) yields
  `WriteError::RawDeviceUnresponsive`; a rejected request or a malformed reply
  yields `WriteError::RawDeviceProtocol`.

### What the app shows

| Feature | How | Status |
|---|---|---|
| Recognition | Standalone inventory, `driver=compx` | **verified on hardware** |
| Battery | Read on every inventory scan into `StandaloneDevice::battery` (appended to the IPC wire, protocol v42); percentage, level bucket, charging state | **verified on hardware** (20 %, 3612 mV) |
| DPI | Existing `read_dpi` / `set_dpi` RPCs; `current` is the active stage's X resolution, the offered range is 50–12 800 in steps of 50 | read and write **verified on hardware** |
| Polling rate | Existing `read_report_rate` / `set_report_rate` RPCs; 125–1000 Hz on a receiver link | read and write **verified on hardware** |
| Sensor, timing, lighting | `read_mouse_settings` / `set_mouse_settings` RPCs; the Pointer tab shows Sensor and Lighting cards (`Capabilities::mouse_settings`) | reads **verified against the hardware dump** in tests; writes untested on hardware |
| SmartShift | Answered as `FeatureUnsupported (0x2110)`, as for a HID++ mouse without the feature | — |
| `openlogi list` | Prints the battery after the device line | **verified on hardware** |

The DPI and polling-rate controls appear because a successful refresh sets
the device's measured `Capabilities::pointer` and `report_rate`. A mouse that
did not answer during the scan keeps no battery and no controls, rather than
showing stale or presumed ones.

### Writes

- DPI writes replace only the active stage's entry (`[x, y, 0, check]` at
  `0x0C + 4·stage`), keeping its colour and every other stage, then re-read
  the flash; a mismatch is `WriteError::DpiNotApplied`. A value the flash
  cannot store is refused before anything is sent (`DpiUnsupported`).
- Polling-rate writes replace the `0x00` pair, then re-read; a mismatch is
  `WriteError::ReportRateNotApplied`; a rate not offered on the link is
  refused first (`ReportRateUnsupported`).
- Sensor, timing and lighting settings are written one change at a time
  (`MouseSettingsChange`), as 2-byte `(value, check)` pairs like the vendor
  app, then the flash is re-read; a change the read-back does not show is
  `WriteError::MouseSettingNotApplied`. Every value is range-checked by its
  type before a session opens. Per change: `0x0A` lift-off; `0xAB`, `0xAF`,
  `0xB1`, `0xB5`, `0xE1` switches; `0xB9` sensor mode; `0xBD` angle (signed
  byte) followed by `0xBF = 1`; `0xAD` and `0xB7` idle timeout, both with the
  same code; `0xA9` button debounce (0..=30 ms, an OpenLogi bound: the vendor
  combo's range was not extracted); `0xE3` wheel debounce; `0x4C`..`0x52`
  DPI LED as four pairs; the body light as one 7-byte record at `0xA0`
  (mode, r, g, b, speed-1, brightness-1, check) plus the pairs `0xA7` and
  `0xB3`. None of these writes has been sent to hardware yet. The DPI LED
  speed is written as the slider position 1..=10 (range unconfirmed).
- Reads of these fields are lenient: a failed check byte or a value the
  vendor app never writes reads as the vendor default, so an unwritten field
  never blocks the DPI and polling-rate reads. The wheel debounce has no
  known default and reads as unset.
- No other flash field is written.

### Tests

- 35 unit tests in `openlogi-device` (`cargo test -p openlogi-device --lib
  compx`): frames and checksums against captured replies, the flash parser
  against the hardware dump, encoder round-trips, and the session against a
  simulated mouse (awake, asleep, ignoring writes, foreign reports
  interleaved, wrong route).
- Wire goldens for the appended field and the two new errors in
  `openlogi-ipc/tests/wire_format.rs`.

### Not yet implemented

- Button remapping and the other flash fields mapped in the [Map](#map) that
  are not sensor, timing or lighting settings (the button encoder exists in
  `config.rs`; no UI or RPC drives it).
- Pairing, long-range mode, receiver LEDs, RSSI and firmware versions in the
  UI (request builders and decoders exist).
- Distinguishing a 4K receiver, to offer 2000–8000 Hz.
