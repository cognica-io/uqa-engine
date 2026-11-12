//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("retired maintenance input read");
    }
}

#[test]
fn maintenance_input_resumes_all_inline_fields_and_retires_terminal_states() {
    let expected = encode(&fixture(), 17);
    for (bytes, steps) in [(1, 7), (17, 1), (4096, 3)] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = StatisticsMaintenance::image_input(&memory, &token).unwrap();
        let mut reader = Cursor::new(&expected);
        let window = ImageDecodeWindow { bytes, steps };
        let mut restored = None;
        for _ in 0..1024 {
            let before = reader.position();
            restored = input.advance(&mut reader, window).unwrap();
            assert!(reader.position() - before <= bytes as u64);
            if restored.is_some() {
                break;
            }
        }
        assert_eq!(restored.unwrap(), fixture());
        assert_eq!(reader.position(), expected.len() as u64);
        assert_eq!(memory.used(), 0);
        assert!(input.advance(&mut NoRead, window).is_err());
    }
    for failure in [
        "cancel", "short", "trailing", "bytes", "steps", "unwind", "drop",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = StatisticsMaintenance::image_input(&memory, &token).unwrap();
        let mut bytes = expected.clone();
        if failure == "short" {
            bytes.pop();
        } else if failure == "trailing" {
            bytes.push(0);
        }
        let mut reader = Cursor::new(bytes);
        assert!(input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 17,
                    steps: 64
                }
            )
            .unwrap()
            .is_none());
        if failure == "drop" {
            drop(input);
            assert_eq!(memory.used(), 0);
            continue;
        }
        if failure == "cancel" {
            token.cancel();
        }
        let window = ImageDecodeWindow {
            bytes: if failure == "bytes" { 0 } else { 17 },
            steps: if failure == "steps" { 0 } else { 64 },
        };
        if failure == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || input.advance(&mut NoRead, window)
            ))
            .is_err());
        } else {
            let mut failed = false;
            for _ in 0..1024 {
                match input.advance(&mut reader, window) {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid maintenance input accepted: {failure}"),
                }
            }
            assert!(failed, "{failure}");
        }
        assert_eq!(memory.used(), 0);
        assert!(input.advance(&mut NoRead, window).is_err());
    }
}
