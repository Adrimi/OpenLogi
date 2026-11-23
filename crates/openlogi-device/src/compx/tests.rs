use super::{
    Battery, FirmwareVersion, IndicatorLed, IndicatorSource, LongRange, Opcode, PAYLOAD_LEN,
    PcDriver, REPORT_LEN, Request, RequestError, Response, ResponseError, SerialNumber, checksum,
};

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("hex byte"))
        .collect()
}

// Replies captured from a 3554:f524 receiver and its paired mouse.
const ATTACH_ACK: &str = "08 02 00 00 00 01 01 00 00 00 00 00 00 00 00 00 49";
const BATTERY_REPLY: &str = "08 04 00 00 00 02 14 00 0e 1c 00 00 00 00 00 00 09";
const VERSION_REPLY: &str = "08 12 00 00 00 02 02 17 00 00 00 00 00 00 00 00 20";
const KEYBOARD_MODE_REJECT: &str = "08 04 01 00 00 80 00 00 00 00 00 00 00 00 00 00 c8";

#[test]
fn checksum_matches_every_captured_reply() {
    for reply in [
        ATTACH_ACK,
        BATTERY_REPLY,
        VERSION_REPLY,
        KEYBOARD_MODE_REJECT,
    ] {
        let bytes = hex(reply);
        assert_eq!(checksum(&bytes[..16]), bytes[16], "{reply}");
    }
}

#[test]
fn handshake_request_matches_the_vendor_frame() {
    let request = Request::pc_driver(PcDriver::Attached);

    assert_eq!(request.as_bytes().as_slice(), hex(ATTACH_ACK).as_slice());
    assert_eq!(
        Request::pc_driver(PcDriver::Detached).as_bytes()[6],
        0x00,
        "detach clears the flag"
    );
}

#[test]
fn battery_reply_decodes_charge_and_voltage() {
    let response = Response::parse(&hex(BATTERY_REPLY)).expect("valid reply");

    assert!(Request::battery().is_answered_by(&response));
    assert_eq!(
        Battery::decode(&response).expect("battery"),
        Battery {
            percent: 20,
            charging: false,
            millivolts: 3612,
        }
    );
}

#[test]
fn version_reply_prints_like_the_vendor_app() {
    let response = Response::parse(&hex(VERSION_REPLY)).expect("valid reply");
    let version = FirmwareVersion::decode(&response).expect("version");

    assert_eq!(version.to_string(), "2.17");
}

#[test]
fn non_zero_status_is_a_rejection() {
    assert_eq!(
        Response::parse(&hex(KEYBOARD_MODE_REJECT)),
        Err(ResponseError::Rejected {
            opcode: Opcode::Battery,
            status: 0x01,
        })
    );
}

#[test]
fn damaged_and_foreign_reports_are_refused() {
    let mut damaged = hex(BATTERY_REPLY);
    damaged[6] = 0x15;
    assert!(matches!(
        Response::parse(&damaged),
        Err(ResponseError::Checksum { .. })
    ));

    let mut foreign = hex(BATTERY_REPLY);
    foreign[0] = 0x01;
    assert_eq!(
        Response::parse(&foreign),
        Err(ResponseError::ForeignReport(0x01))
    );

    assert_eq!(
        Response::parse(&hex(BATTERY_REPLY)[..16]),
        Err(ResponseError::Truncated(16))
    );
}

#[test]
fn unknown_opcode_is_an_error_not_a_guess() {
    let mut frame = vec![0u8; REPORT_LEN];
    frame[0] = 0x08;
    frame[1] = 0x7e;
    frame[16] = checksum(&frame[..16]);

    assert_eq!(
        Response::parse(&frame),
        Err(ResponseError::UnknownOpcode(0x7e))
    );
}

#[test]
fn typed_decoders_refuse_another_commands_reply() {
    let version = Response::parse(&hex(VERSION_REPLY)).expect("valid reply");

    assert_eq!(
        Battery::decode(&version),
        Err(ResponseError::WrongReply {
            expected: Opcode::Battery,
            found: Opcode::ReceiverVersion,
        })
    );
    assert!(!Request::battery().is_answered_by(&version));
}

fn reply(opcode: Opcode, payload: &[u8]) -> Response {
    let mut frame = [0u8; REPORT_LEN];
    frame[0] = 0x08;
    frame[1] = opcode.into();
    frame[5] = u8::try_from(payload.len()).expect("short payload");
    frame[6..6 + payload.len()].copy_from_slice(payload);
    frame[16] = checksum(&frame[..16]);
    Response::parse(&frame).expect("valid reply")
}

#[test]
fn out_of_range_battery_values_are_unsupported() {
    assert_eq!(
        Battery::decode(&reply(Opcode::Battery, &[101, 0, 0x0e, 0x1c])),
        Err(ResponseError::UnsupportedValue {
            opcode: Opcode::Battery,
            value: 101,
        })
    );
    assert_eq!(
        Battery::decode(&reply(Opcode::Battery, &[50, 2, 0x0e, 0x1c])),
        Err(ResponseError::UnsupportedValue {
            opcode: Opcode::Battery,
            value: 2,
        })
    );
}

#[test]
fn long_range_and_indicator_round_trip() {
    assert_eq!(Request::set_long_range(LongRange::On).as_bytes()[6], 1);
    assert_eq!(
        LongRange::decode(&reply(Opcode::LongRange, &[1])),
        Ok(LongRange::On)
    );
    assert!(
        LongRange::decode(&reply(Opcode::LongRange, &[2])).is_err_and(|error| {
            error
                == ResponseError::UnsupportedValue {
                    opcode: Opcode::LongRange,
                    value: 2,
                }
        })
    );

    let led = IndicatorLed {
        source: IndicatorSource::Battery,
        parameters: [0x10, 0x20],
    };
    let request = Request::set_indicator_led(led);
    assert_eq!(&request.as_bytes()[5..9], &[3, 2, 0x10, 0x20]);
    assert_eq!(
        IndicatorLed::decode(&reply(Opcode::IndicatorLed, &[2, 0x10, 0x20])),
        Ok(led)
    );
}

#[test]
fn flash_requests_carry_address_and_length() {
    let read = Request::read_flash(0x00a9, 10).expect("read");
    assert_eq!(&read.as_bytes()[..6], &[0x08, 0x08, 0x00, 0x00, 0xa9, 10]);

    let write = Request::write_flash(0x00a9, &[0x08, 0x4d]).expect("write");
    assert_eq!(
        &write.as_bytes()[..8],
        &[0x08, 0x07, 0x00, 0x00, 0xa9, 2, 0x08, 0x4d]
    );
    assert_eq!(write.as_bytes()[16], checksum(&write.as_bytes()[..16]));
}

#[test]
fn flash_transfers_are_bounded() {
    assert_eq!(
        Request::read_flash(0, 0),
        Err(RequestError::EmptyFlashTransfer)
    );
    assert_eq!(
        Request::read_flash(0, 11),
        Err(RequestError::PayloadTooLong(11))
    );
    assert_eq!(
        Request::write_flash(0, &[]),
        Err(RequestError::EmptyFlashTransfer)
    );
    assert_eq!(
        Request::write_flash(0, &[0; PAYLOAD_LEN + 1]),
        Err(RequestError::PayloadTooLong(PAYLOAD_LEN + 1))
    );
}

#[test]
fn flash_replies_match_only_their_own_chunk() {
    let request = Request::read_flash(0x0a, 10).expect("read");
    let mut frame = [0u8; REPORT_LEN];
    frame[0] = 0x08;
    frame[1] = 0x08;
    frame[4] = 0x14;
    frame[5] = 10;
    frame[16] = checksum(&frame[..16]);
    let other_chunk = Response::parse(&frame).expect("valid reply");

    assert!(!request.is_answered_by(&other_chunk));
}

#[test]
fn pairing_and_binding_follow_the_vendor_layout() {
    let pairing = Request::enter_pairing(0x07);
    assert_eq!(&pairing.as_bytes()[5..9], &[2, 0, 0, 0x07]);

    let bind = Request::bind_receiver(0x1234);
    assert_eq!(&bind.as_bytes()[14..16], &[0x34, 0x12]);
    assert_eq!(bind.as_bytes()[16], checksum(&bind.as_bytes()[..16]));
}

fn serial_reply(payload: [u8; PAYLOAD_LEN]) -> Response {
    let mut frame = [0u8; REPORT_LEN];
    frame[0] = 0x08;
    frame[1] = 0x20;
    frame[5] = 10;
    frame[6..16].copy_from_slice(&payload);
    frame[16] = checksum(&frame[..16]);
    Response::parse(&frame).expect("valid frame")
}

#[test]
fn serial_reply_keeps_all_ten_payload_bytes() {
    let response = serial_reply([0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0xab, 0xcd]);

    assert!(Request::serial_number().is_answered_by(&response));
    let serial = SerialNumber::decode(&response)
        .expect("serial reply")
        .expect("an identity");
    assert_eq!(serial.to_string(), "3132333435363738abcd");
}

#[test]
fn blank_serials_are_no_identity() {
    for blank in [[0x00; PAYLOAD_LEN], [0xff; PAYLOAD_LEN]] {
        assert_eq!(SerialNumber::decode(&serial_reply(blank)), Ok(None));
    }
    let mostly_blank = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe];
    assert!(matches!(
        SerialNumber::decode(&serial_reply(mostly_blank)),
        Ok(Some(_))
    ));
}

#[test]
fn serial_decoder_refuses_another_reply() {
    let response = Response::parse(&hex(BATTERY_REPLY)).expect("valid reply");

    assert_eq!(
        SerialNumber::decode(&response),
        Err(ResponseError::WrongReply {
            expected: Opcode::SerialNumber,
            found: Opcode::Battery,
        })
    );
}
