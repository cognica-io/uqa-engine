//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("borrowed JSON must not reread its source")
    }
}

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

#[test]
fn borrowed_json_continuation_bounds_long_tokens_and_preserves_original_ranges() {
    let text = format!(
        "{}{{\"a\":[\"{}\\uD83D\\uDE03\",{}],\"a\":null}}",
        " ".repeat(32768),
        "한🙂".repeat(8192),
        "9".repeat(65536)
    );
    let length = text.len();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input =
        ImageDecoder::<_, JsonReadError>::new(&memory, &token, |mut channel| async move {
            let mut work = channel.input();
            let mut json = JsonReader::from_slice(text.as_bytes(), work.memory, work.cancellation);
            let mut reference = JsonReader::new(&text, work.memory, work.cancellation);
            let mut count = 0;
            while let Some(event) = json.next_event_from(&mut work).await? {
                match event.token {
                    JsonToken::String(bytes) | JsonToken::Key(bytes) | JsonToken::Number(bytes) => {
                        assert_eq!(bytes.as_ptr(), text[event.range.clone()].as_ptr());
                        assert_eq!(bytes, &text.as_bytes()[event.range.clone()]);
                    }
                    _ => {}
                }
                assert_eq!(Some(event.text_number()?), reference.next_event()?);
                count += 1;
            }
            assert!(reference.next_event()?.is_none());
            Ok(count)
        })
        .unwrap();
    let mut polls = 0;
    let count = loop {
        polls += 1;
        if let Some(count) = input.advance(&mut NoRead, WINDOW).unwrap() {
            break count;
        }
    };
    assert_eq!(count, 9);
    assert!(polls > length / 4096);
    assert_eq!(memory.used(), 0);
}

#[test]
fn borrowed_string_continuation_preserves_unicode_capacity_and_failure_retirement() {
    let raw = format!(
        "\"{}\\n{}\\uD83D\\uDE03\"",
        "한🙂".repeat(600),
        "x".repeat(16384)
    );
    let expected: String = serde_json::from_str(&raw).unwrap();
    // The original constructor copies these two literal runs and the two escaped scalars.
    let original_memory = MemoryBudget::new(1 << 20);
    let original_token = CancellationToken::new();
    let mut original = crate::memory::ProductionString::new(ProductionControl::new(
        &original_memory,
        &original_token,
        &original_token,
    ));
    original.push_str(&"한🙂".repeat(600)).unwrap();
    original.push('\n').unwrap();
    original.push_str(&"x".repeat(16384)).unwrap();
    original.push('😃').unwrap();
    let original = original.finish().unwrap().into_budgeted().unwrap();
    let expected_capacity = original.capacity();
    drop(original);
    assert_eq!(original_memory.used(), 0);
    for mode in ["complete", "cancel", "memory", "window", "drop"] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let retained = decode_json_string(br#""prior""#, &memory, &token).unwrap();
        let retained_bytes = memory.used();
        let synchronous = decode_json_string(&raw, &memory, &token).unwrap();
        assert_eq!(synchronous.capacity(), expected_capacity);
        drop(synchronous);
        let bytes = raw.as_bytes().to_vec();
        let mut input =
            ImageDecoder::<_, JsonReadError>::new(&memory, &token, |mut channel| async move {
                decode_json_string_from(&bytes, &mut channel.input()).await
            })
            .unwrap();
        let header_bytes = memory.used();
        while memory.used() == header_bytes {
            assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
        }
        if mode == "complete" {
            let value = input.read(&mut NoRead).unwrap();
            assert_eq!(&**value, expected);
            assert_eq!(value.capacity(), expected_capacity);
            drop(value);
        } else if mode == "drop" {
            drop(input);
        } else {
            let full = if mode == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            if mode == "cancel" {
                token.cancel();
            }
            let window = ImageDecodeWindow {
                bytes: usize::from(mode != "window"),
                steps: 1,
            };
            let error = loop {
                match input.advance(&mut NoRead, window) {
                    Ok(None) => {}
                    Ok(Some(_)) => panic!("unpublished string unexpectedly completed: {mode}"),
                    Err(error) => break error,
                }
            };
            match mode {
                "cancel" => assert!(matches!(error, JsonReadError::Cancelled(_))),
                "memory" => assert!(matches!(error, JsonReadError::Memory(_))),
                _ => assert!(matches!(error, JsonReadError::InvalidJson)),
            }
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
            drop((input, full));
        }
        assert_eq!(&**retained, "prior");
        assert_eq!(memory.used(), retained_bytes, "{mode}");
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
}
