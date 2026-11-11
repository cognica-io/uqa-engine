//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::ImageDecodeWindow;
use std::io::{Cursor, Read};

struct Source {
    bytes: Cursor<Vec<u8>>,
    calls: usize,
}

impl Read for Source {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        self.bytes.read(output)
    }
}

#[test]
fn native_value_input_keeps_synchronous_and_retained_frames_bounded() {
    let images: Vec<_> = fixtures().iter().map(encode).collect();
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            for bytes in images {
                let value = read_value_image(&mut bytes.as_slice(), &memory, &token).unwrap();
                assert_eq!(encode(&value), bytes);
                drop(value);
                let input = ValueImageInput::new(&memory, &token).unwrap();
                let value = input.read(&mut bytes.as_slice()).unwrap();
                assert_eq!(encode(&value), bytes);
                drop(value);
                assert_eq!(memory.used(), 0);
            }
            let bytes = encode(&Value::Float(-0.0));
            let memory = MemoryBudget::new(0);
            let allocations = allocation_counter::measure(|| {
                let value = read_value_image(&mut bytes.as_slice(), &memory, &token).unwrap();
                assert!(
                    matches!(*value, Value::Float(value) if value.to_bits() == (-0.0_f64).to_bits())
                );
            });
            assert_eq!(allocations.count_total, 0);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_value_input_preserves_every_carrier_with_independent_byte_and_work_windows() {
    let mut values = fixtures();
    values.push(Value::List(vec![
        Value::Str("λ한😀".repeat(2049)),
        Value::Bytes(vec![13; 8193]),
        Value::Map(BTreeMap::from([("field".into(), Value::List(fixtures()))])),
    ]));
    let mut nested = Value::Null;
    for _ in 0..MAX_DEPTH {
        nested = Value::List(vec![nested]);
    }
    values.push(nested);
    for value in values {
        let expected = encode(&value);
        for (width, steps) in [(1, 1), (7, 3), (4096, 5)] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut bytes = expected.clone();
            bytes.extend_from_slice(&[0xaa, 0xbb]);
            let mut source = Source {
                bytes: Cursor::new(bytes),
                calls: 0,
            };
            let mut input = ValueImageInput::new(&memory, &token).unwrap();
            let result = loop {
                let position = source.bytes.position();
                let calls = source.calls;
                let output = input
                    .advance(
                        &mut source,
                        ImageDecodeWindow {
                            bytes: width,
                            steps,
                        },
                    )
                    .unwrap();
                assert!(source.bytes.position() - position <= width as u64);
                assert!(source.calls - calls <= steps);
                if let Some(value) = output {
                    break value;
                }
            };
            assert_eq!(source.bytes.position(), expected.len() as u64);
            drop(source);
            assert_eq!(memory.used(), result.reserved_bytes());
            assert_eq!(encode(&result), expected);
            token.cancel();
            assert!(input
                .advance(&mut &[][..], ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
            assert_eq!(encode(&result), expected);
            drop(result);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_value_input_retires_partial_nested_payloads_between_calls() {
    let value = Value::Record(RecordValue::from_parts(
        vec![
            ("first".into(), Value::Str("한".repeat(2049))),
            ("second".into(), Value::Bytes(vec![42; 8193])),
        ],
        Some(12001),
    ));
    let bytes = encode(&value);
    for mode in ["cancel", "memory", "drop", "truncated"] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut source = Cursor::new(bytes.clone());
        if mode == "truncated" {
            source.get_mut().truncate(bytes.len() - 1);
        }
        let mut input = ValueImageInput::new(&memory, &token).unwrap();
        let window = ImageDecodeWindow {
            bytes: 31,
            steps: 5,
        };
        while source.position() < 100 {
            assert!(input.advance(&mut source, window).unwrap().is_none());
        }
        assert!(memory.used() > 0);
        if mode == "drop" {
            drop(input);
        } else {
            let occupied = (mode == "memory").then(|| memory.reserve(memory.available()).unwrap());
            if mode == "cancel" {
                token.cancel();
            }
            let error = loop {
                match input.advance(&mut source, window) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("accepted {mode}"),
                    Err(error) => break error,
                }
            };
            match mode {
                "cancel" => assert!(matches!(error, ValueImageError::Cancelled(_))),
                "memory" => assert!(matches!(error, ValueImageError::Memory(_))),
                "truncated" => assert!(matches!(error, ValueImageError::Io(_))),
                _ => unreachable!(),
            }
            drop(occupied);
            assert!(input.advance(&mut source, window).is_err());
        }
        assert_eq!(memory.used(), 0, "{mode}");
    }
}
