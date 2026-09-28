//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn whole_stream() -> Vec<u8> {
    let fixture = fixture();
    format!(
        "{}: heartbeat\n\n{}{}",
        fixture.ready, fixture.notification, fixture.closed
    )
    .into_bytes()
}

fn decode_complete(chunks: &[&[u8]]) -> Vec<NotificationWireEvent> {
    let mut decoder = decoder();
    let mut output = Vec::new();
    for chunk in chunks {
        feed(&mut decoder, chunk, &mut output).unwrap();
    }
    assert_eq!(decoder.finish().unwrap(), None);
    output
}

#[test]
fn every_byte_split_preserves_utf8_json_line_endings_and_bom() {
    let expected = decode_complete(&[&whole_stream()]);
    let text = String::from_utf8(whole_stream()).unwrap();
    for ending in ["\n", "\r\n", "\r"] {
        for prefix in ["", "\u{feff}"] {
            let input = format!("{prefix}{}", text.replace('\n', ending)).into_bytes();
            for split in 0..=input.len() {
                assert_eq!(
                    decode_complete(&[&input[..split], &input[split..]]),
                    expected,
                    "split {split}, ending {ending:?}, BOM {}",
                    !prefix.is_empty()
                );
            }
            let one_byte: Vec<_> = input.chunks(1).collect();
            assert_eq!(decode_complete(&one_byte), expected);
        }
    }
}

#[test]
fn multiline_data_first_colon_one_space_and_last_event_field_follow_sse() {
    let fixture = fixture();
    let ready =
        fixture
            .ready
            .replacen("data: {", "event: ignored\nevent: ready\ndata: {\ndata:", 1);
    let ready = ready.replacen(",\"delivery\"", ",\ndata: \"delivery\"", 1);
    let notification = fixture.notification.replacen("data: ", "data:", 1);
    let stream = format!("{ready}{notification}{}", fixture.closed);
    assert_eq!(
        decode_complete(&[stream.as_bytes()]),
        decode_complete(&[format!(
            "{}{}{}",
            fixture.ready, fixture.notification, fixture.closed
        )
        .as_bytes()])
    );
    let first_unknown = fixture
        .ready
        .replacen("event: ready", "event: ignored\nevent: ready", 1);
    assert!(decoder().decode(first_unknown.as_bytes()).is_ok());
    for prefix in ["event:  ready", "Event: ready", "event:\tready"] {
        fail_frame(
            fixture.ready.replacen("event: ready", prefix, 1).as_bytes(),
            ProtocolError::InvalidFields,
            false,
        );
    }
    for prefix in ["id: cursor\n", "id:\n", "retry: 100\n", "unsupported\n"] {
        fail_frame(
            format!("{prefix}{}", fixture.ready).as_bytes(),
            ProtocolError::InvalidFields,
            false,
        );
    }
    for input in [
        b"event: ready\n\n".as_slice(),
        b"data: {}\n\n",
        b"event\n data: {}\n\n",
    ] {
        fail_frame(input, ProtocolError::InvalidFields, false);
    }
    fail_frame(
        fixture
            .ready
            .replacen("event: ready", "event: ready\nevent:", 1)
            .as_bytes(),
        ProtocolError::InvalidFields,
        false,
    );
}

fn padded(frame: &str, length: usize, ending: &str) -> Vec<u8> {
    let frame = frame.replace('\n', ending);
    let padding = length - frame.len() - 1 - ending.len();
    format!(":{}{ending}{frame}", "x".repeat(padding)).into_bytes()
}

#[test]
fn complete_event_limit_includes_fields_delimiters_and_crlf() {
    for ending in ["\n", "\r\n", "\r"] {
        let wire = padded(&fixture().ready, MAX_NOTIFICATION_WIRE_BYTES, ending);
        assert_eq!(wire.len(), MAX_NOTIFICATION_WIRE_BYTES);
        let mut decoder = decoder();
        let mut output = Vec::new();
        feed(&mut decoder, &wire, &mut output).unwrap();
        if ending == "\r" {
            assert!(output.is_empty());
            output.extend(decoder.finish().unwrap());
            assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEnd));
        }
        assert!(matches!(
            output.as_slice(),
            [NotificationWireEvent::Ready(_)]
        ));
        let oversized = padded(&fixture().ready, MAX_NOTIFICATION_WIRE_BYTES + 1, ending);
        fail_frame(&oversized, ProtocolError::ByteLimit, false);
    }
}

#[test]
fn exact_limit_final_cr_does_not_expose_a_frame_before_its_optional_lf() {
    let wire = padded(&fixture().ready, MAX_NOTIFICATION_WIRE_BYTES, "\r");
    let mut decoder = decoder();
    assert!(decoder.decode(&wire).unwrap().event.is_none());
    assert!(decoder.ready().is_none());
    assert_eq!(decoder.decode(b"\n").unwrap_err(), ProtocolError::ByteLimit);
    assert!(decoder.ready().is_none());
    let mut decoder = super::decoder();
    assert!(decoder.decode(&wire).unwrap().event.is_none());
    let notification = fixture().notification;
    let ready = decoder.decode(notification.as_bytes()).unwrap();
    assert_eq!(ready.consumed, 0);
    assert!(matches!(ready.event, Some(NotificationWireEvent::Ready(_))));
    assert!(matches!(
        decoder.decode(notification.as_bytes()).unwrap().event,
        Some(NotificationWireEvent::Notification(_))
    ));
}

#[test]
fn last_crlf_byte_is_charged_to_the_previous_frame() {
    let wire = padded(&fixture().ready, MAX_NOTIFICATION_WIRE_BYTES, "\r\n");
    let mut decoder = decoder();
    let step = decoder.decode(&wire[..wire.len() - 1]).unwrap();
    assert!(matches!(step.event, Some(NotificationWireEvent::Ready(_))));
    let step = decoder.decode(&wire[wire.len() - 1..]).unwrap();
    assert_eq!(step.consumed, 1);
    assert!(step.event.is_none());
    let terminal = fixture().closed.replace('\n', "\r\n");
    let mut output = Vec::new();
    feed(&mut decoder, terminal.as_bytes(), &mut output).unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(decoder.finish().unwrap(), None);
}

#[test]
fn unfinished_frames_and_comment_blocks_have_the_same_bound() {
    for input in [
        vec![b'x'; MAX_NOTIFICATION_WIRE_BYTES + 1],
        format!(":{}", "x".repeat(MAX_NOTIFICATION_WIRE_BYTES)).into_bytes(),
        b": short comment\n".repeat(MAX_NOTIFICATION_WIRE_BYTES / 16 + 1),
    ] {
        fail_frame(&input, ProtocolError::ByteLimit, false);
    }
    let mut decoder = decoder();
    assert!(decoder
        .decode(&vec![b'x'; MAX_NOTIFICATION_WIRE_BYTES])
        .unwrap()
        .event
        .is_none());
    assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEnd));
    let heartbeat = format!(":{}\n\n", "x".repeat(MAX_NOTIFICATION_WIRE_BYTES - 3));
    let step = super::decoder().decode(heartbeat.as_bytes()).unwrap();
    assert_eq!(step.event, Some(NotificationWireEvent::Heartbeat));
}

#[test]
fn cumulative_streams_exceed_the_frame_bound_without_a_chunk_copy_or_event_queue() {
    let fixture = fixture();
    let mut input = fixture.ready.clone().into_bytes();
    for sequence in 1..=1_000 {
        let mut body = notification_body();
        body["sequence"] = Value::String(sequence.to_string());
        input.extend(event("notification", &body));
    }
    input.extend(fixture.closed.as_bytes());
    assert!(input.len() > MAX_NOTIFICATION_WIRE_BYTES);
    let mut decoder = decoder();
    let first = decoder.decode(&input).unwrap();
    assert_eq!(first.consumed, fixture.ready.len());
    assert!(matches!(first.event, Some(NotificationWireEvent::Ready(_))));
    let mut output = Vec::new();
    feed(&mut decoder, &input[first.consumed..], &mut output).unwrap();
    assert_eq!(output.len(), 1_001);
    for (index, event) in output.iter().take(1_000).enumerate() {
        assert!(
            matches!(event, NotificationWireEvent::Notification(NotificationEvent::Notification { sequence, .. }) if *sequence == index as u64 + 1)
        );
    }
    assert_eq!(decoder.finish().unwrap(), None);
}

#[test]
fn malformed_utf8_is_rejected_in_fields_data_and_comments() {
    for input in [
        b"eve\xffnt: ready\ndata: {}\n\n".as_slice(),
        b": \xff\n\n",
        b"event: notification\ndata: {\"payload\":\"\xff\"}\n\n",
        b"\xef\xbbX\n\n",
    ] {
        for split in 0..=input.len() {
            let mut decoder = ready_decoder();
            let mut output = Vec::new();
            let result = feed(&mut decoder, &input[..split], &mut output)
                .and_then(|()| feed(&mut decoder, &input[split..], &mut output));
            assert_eq!(result, Err(ProtocolError::InvalidUTF8));
            assert!(output.is_empty());
        }
    }
    fail_frame(
        format!("\u{feff}\u{feff}{}", fixture().ready).as_bytes(),
        ProtocolError::InvalidFields,
        false,
    );
}

#[test]
fn eof_discards_incomplete_events_and_distinguishes_corruption_from_loss() {
    let fixture = fixture();
    for cut in 0..fixture.ready.len() {
        let mut decoder = decoder();
        assert!(decoder
            .decode(&fixture.ready.as_bytes()[..cut])
            .unwrap()
            .event
            .is_none());
        assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEnd));
        assert!(decoder.ready().is_none());
    }
    for suffix in [
        b"\xef".as_slice(),
        b"\xef\xbb",
        b"event: notification\ndata: {\"payload\":\"\xf0\x9f",
    ] {
        let mut decoder = decoder();
        assert!(decoder.decode(suffix).unwrap().event.is_none());
        assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEnd));
    }
    let mut decoder = decoder();
    assert!(decoder.decode(b":\xff").unwrap().event.is_none());
    assert_eq!(decoder.finish(), Err(ProtocolError::InvalidUTF8));
    let mut decoder = ready_decoder();
    assert_eq!(decoder.finish(), Err(ProtocolError::UnexpectedEnd));
}
