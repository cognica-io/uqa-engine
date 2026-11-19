//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

struct Counted<'a> {
    reader: Cursor<&'a [u8]>,
    calls: usize,
}
impl Read for Counted<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        self.reader.read(bytes)
    }
}
struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("retired statistics input read");
    }
}

#[test]
fn statistics_input_resumes_distributions_and_ordered_utf8_names() {
    let stats = ColumnStats {
        min_value: Some(Value::Str("한글".repeat(1024))),
        mcv_values: vec![Value::List(vec![Value::Int(7), Value::Null])],
        mcv_frequencies: vec![-0.0, f64::from_bits(0x7ff8_0000_0000_1234)],
        ..ColumnStats::default()
    };
    let source: ColumnStatistics = [
        (format!("{}a", "분포".repeat(1024)), stats.clone()),
        (format!("{}b", "분포".repeat(1024)), stats),
    ]
    .into_iter()
    .collect();
    let expected = encode(&source, 31);
    drop(source);
    for (bytes, steps) in [(1, 7), (17, 1), (4096, 3)] {
        let memory = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let window = ImageDecodeWindow { bytes, steps };
        let mut input = ColumnStatistics::image_input(&memory, &token).unwrap();
        let mut reader = Counted {
            reader: Cursor::new(expected.as_slice()),
            calls: 0,
        };
        let mut restored = None;
        for _ in 0..expected.len() * 8 + 1024 {
            let before = reader.reader.position();
            let calls = reader.calls;
            restored = input.advance(&mut reader, window).unwrap();
            assert!(reader.reader.position() - before <= bytes as u64);
            assert!(reader.calls - calls <= steps);
            if restored.is_some() {
                break;
            }
        }
        let restored = restored.expect("statistics completed");
        assert_eq!(reader.reader.position(), expected.len() as u64);
        assert!(input.advance(&mut NoRead, window).is_err());
        token.cancel();
        assert_eq!(encode(&restored, 13), expected);
        let shared = restored.clone();
        drop(restored);
        assert!(memory.used() > 0);
        drop(shared);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn statistics_input_retires_partial_values_on_faults_and_invalid_windows() {
    let expected = encode(&fixture(), 17);
    for failure in [
        "cancel", "memory", "short", "trailing", "bytes", "steps", "unwind", "drop",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = ColumnStatistics::image_input(&memory, &token).unwrap();
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
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
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
            for _ in 0..expected.len() * 8 + 1024 {
                match input.advance(&mut reader, window) {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid statistics input accepted: {failure}"),
                }
            }
            assert!(failed, "{failure}");
        }
        drop(full);
        assert_eq!(memory.used(), 0);
        assert!(input.advance(&mut NoRead, window).is_err());
    }
}
