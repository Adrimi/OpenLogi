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
| `0x20` | Read serial | none | serial bytes | D |
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
hardware-verified**. Writes have **not** been sent to hardware yet.

The image is 0xF0 bytes, read as 24 chunks of 10 bytes with opcode `0x08`.

Encoding rules:

- Each scalar is stored as a pair `(value, 0x55 - value)`.
- Each table entry is its data bytes followed by one check byte
  `0x55 - sum(data bytes)`.

### Map

| Offset | Content |
|---|---|
| `0x00` | polling rate (scalar pair) |
| `0x02` | DPI stage count, 1-8 (scalar pair) |
| `0x04` | active stage, zero-based, less than count (scalar pair) |
| `0x06`, `0x08`, `0x0A` | unidentified scalars |
| `0x0C` | eight DPI entries `[x, y, high-bits, check]` |
| `0x2C` | eight colour entries `[r, g, b, check]` |
| `0x4C`-`0x5F` | lighting (unidentified) |
| `0x60` | sixteen button entries `[kind, a, b, check]` |
| `0xA9` | debounce, ms |
| `0xAB`-`0xED` | unidentified; likely lift-off distance, ripple control, angle snapping, motion sync, sleep time, lighting-off timers |
| `0xC0`-`0xEF` | all `0xFF` on the test mouse |

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

Inferred; it matches the vendor defaults and the hardware dump but is **not
hardware-confirmed**. When the high-bits byte is 0, `dpi = (byte + 1) * 50`.
Values above 12800 need the high-bits byte, whose layout is unknown.

### Button kinds

From the vendor catalogue (D):

| Kind | Meaning | `a` |
|---|---|---|
| 0 | disabled | |
| 1 | mouse button | `0x01` left, `0x02` right, `0x04` middle, `0x08` back, `0x10` forward |
| 2 | DPI | 1 cycle, 2 up, 3 down |
| 3 | horizontal scroll | 1 left, 2 right |
| `0x0B` | vertical scroll | 1 up, 2 down |
| 4 | fire button | |
| 5 | shortcut / combo key | |
| 6 | macro | |
| 7 | polling-rate cycle | |

Multimedia keys exist in the catalogue with HID consumer usages: `0xCD` play/pause,
`0xB5` next, `0xB6` previous, `0xB7` stop, `0xE2` mute, `0xE9` volume up, `0xEA`
volume down, `0x183` media player, `0x18A` email, `0x192` calculator, `0x194` my
computer, `0x221`-`0x22A` browser keys. Their stored kind byte is **not
confirmed**; one stored entry on hardware was `08 04 00` (see below).

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

Captured from the test mouse (**verified on hardware**). Under the inferred
encoding it holds 5 DPI stages (800, 1000, 1200, 1400, 1600), active stage 3
(stored zero-based value `03`, i.e. the 1400 DPI stage), 1000 Hz and
debounce 8 ms.

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
| `0x06`, `0x08` | `00 55`, `00 55` | unidentified scalars, value 0 |
| `0x0A` | `01 54` | unidentified scalar, value 1 |
| `0x0C` | `0f 0f 00 37` | stage 0: 16 * 50 = 800 DPI |
| `0x10` | `13 13 00 2f` | stage 1: 20 * 50 = 1000 DPI |
| `0x14` | `17 17 00 27` | stage 2: 24 * 50 = 1200 DPI |
| `0x18` | `1b 1b 00 1f` | stage 3: 28 * 50 = 1400 DPI |
| `0x1C` | `1f 1f 00 17` | stage 4: 32 * 50 = 1600 DPI |
| `0x20`-`0x2B` | `7f 7f 00 57` x3 | unused DPI entries, 6400 DPI placeholder under the inferred encoding |
| `0x2C` | `ff 00 00 56` | colour 0: red |
| `0x30` | `00 ff 00 56` | colour 1: green |
| `0x34` | `ff ff 00 57` | colour 2: yellow |
| `0x38` | `00 00 ff 56` | colour 3: blue |
| `0x3C` | `ff 80 00 d6` | colour 4: orange |
| `0x40` | `00 ff ff 57` | colour 5: cyan |
| `0x44` | `00 ff ff 57` | colour 6: cyan (as stored) |
| `0x48` | `ff ff ff 58` | colour 7: white |
| `0x4C`-`0x5F` | `02 53 ff 56 03 52 00 55 ff 00 ff 57 00 55 80 d5 03 52 00 55` | lighting, unidentified |
| `0x60` | `01 01 00 53` | button 0: mouse button, left |
| `0x64` | `01 02 00 52` | button 1: mouse button, right |
| `0x68` | `01 04 00 50` | button 2: mouse button, middle |
| `0x6C` | `01 08 00 4c` | button 3: mouse button, back |
| `0x70` | `01 10 00 44` | button 4: mouse button, forward |
| `0x74` | `02 01 00 52` | button 5: DPI cycle |
| `0x78` | `02 02 00 51` | button 6: DPI up |
| `0x7C` | `02 03 00 50` | button 7: DPI down |
| `0x80` | `07 00 00 4e` | button 8: polling-rate cycle |
| `0x84` | `08 04 00 49` | button 9: kind `0x08` (unconfirmed, likely multimedia) |
| `0x88` | `04 0a 03 44` | button 10: fire button |
| `0x8C`-`0x9F` | `00 00 00 55` x5 | buttons 11-15: disabled |
| `0xA0`-`0xA7` | `01 ff 00 ff 07 09 46 00` | unidentified |
| `0xA8` | `55` | unidentified |
| `0xA9` | `08 4d` | debounce 8 ms, check `0x55 - 0x08 = 0x4d` |
| `0xAB`-`0xBD` | `01 54 06 4f 00 55 ...` | scalar pairs, unidentified |
| `0xC0`-`0xEF` | `ff` | unused |

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

- DPI high-bits byte: layout for values above 12800 DPI (and whether the
  `(byte + 1) * 50` rule is exact) is unknown.
- Unidentified fields: scalars at `0x06`/`0x08`/`0x0A`, the lighting block
  `0x4C`-`0x5F`, and `0xAB`-`0xED` (likely lift-off distance, ripple control, angle
  snapping, motion sync, sleep time, lighting-off timers).
- Payload of opcode `0x03` (online) is not decoded.
- Charging flag (`0x04` `payload[1]`): only the value 0 has been observed.
- Stored kind byte for multimedia buttons; the observed entry `08 04 00` is not
  mapped.
- Write path (`0x07`): implemented for the active stage's DPI entry and the
  polling rate, each read back after the write, but not yet exercised on
  hardware. Whether the firmware needs a commit or reload step after a
  write is unconfirmed; a read-back mismatch would surface as
  `DpiNotApplied` / `ReportRateNotApplied`.
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
| DPI | Existing `read_dpi` / `set_dpi` RPCs; `current` is the active stage's X resolution, the offered range is 50–12 800 in steps of 50 | read **verified on hardware**; write not yet |
| Polling rate | Existing `read_report_rate` / `set_report_rate` RPCs; 125–1000 Hz on a receiver link | read **verified on hardware**; write not yet |
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

- Button remapping, DPI stage list and colours, debounce, lighting and the
  unidentified flash fields — the encoders exist in `config.rs`, but no UI or
  RPC drives them.
- Pairing, long-range mode, receiver LEDs, RSSI and firmware versions in the
  UI (request builders and decoders exist).
- Distinguishing a 4K receiver, to offer 2000–8000 Hz.
