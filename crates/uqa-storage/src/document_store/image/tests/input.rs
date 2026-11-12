//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::ImageDecodeWindow;

fn fixture() -> Vec<u8> {
    // Independent tuple: xmin 42, one UTF-8 field, signed integer -9.
    let mut bytes = b"UQADOC01\x01\x2a\0\0\0UQAVAL01\x11".to_vec();
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&6_u64.to_le_bytes());
    bytes.extend_from_slice("한글".as_bytes());
    bytes.push(3);
    bytes.extend_from_slice(&(-9_i64).to_le_bytes());
    bytes
}

#[test]
fn document_input_bounds_each_call_and_preserves_embedded_tuple_boundaries() {
    let bytes = fixture();
    for maximum in [1, 3, 17, 4096] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut stream = bytes.clone();
        stream.extend_from_slice(b"following tuple");
        let mut source = Cursor::new(stream);
        let mut input = document_image_input(&control).unwrap();
        let mut calls = 0;
        let value = loop {
            let before = source.position();
            calls += 1;
            let window = ImageDecodeWindow {
                bytes: maximum,
                steps: if calls % 2 == 0 { 1 } else { 3 },
            };
            let value = input.advance(&mut source, window).unwrap();
            assert!(source.position() - before <= maximum as u64);
            if let Some(value) = value {
                break value;
            }
            assert!(calls < 1000);
        };
        assert_eq!(source.position(), bytes.len() as u64);
        assert_eq!(value.metadata(), DocumentMetadata::with_tuple_xmin(42));
        assert_eq!(value.fields()["한글"], Value::Int(-9));
        assert_eq!(&*value, &*decode_document_image(&bytes, &control).unwrap());
        assert!(input
            .advance(&mut source, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        drop((value, input));
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(0);
    let mut empty = b"UQADOC01\0UQAVAL01\x11".to_vec();
    empty.extend_from_slice(&0_u64.to_le_bytes());
    let allocation = allocation_counter::measure(|| {
        assert!(decode_document_image(&empty, &control)
            .unwrap()
            .fields()
            .is_empty());
    });
    assert_eq!(allocation.count_total, 0);
}

#[test]
fn document_input_rejects_truncation_and_retires_partial_payloads() {
    let bytes = fixture();
    for end in 0..bytes.len() {
        let control = StorageReadControl::with_limit(1 << 20);
        let input = document_image_input(&control).unwrap();
        assert!(input.read(&mut &bytes[..end]).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    for failure in ["cancel", "window", "drop", "io", "unwind"] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = document_image_input(&control).unwrap();
        let mut source = Cursor::new(&bytes);
        assert!(input
            .advance(
                &mut source,
                ImageDecodeWindow {
                    bytes: 1,
                    steps: 64
                }
            )
            .unwrap()
            .is_none());
        if failure == "drop" {
            drop(input);
        } else if failure == "unwind" {
            struct Unwind;
            impl Read for Unwind {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    panic!("tuple reader unwind")
                }
            }
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                input.advance(
                    &mut Unwind,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: 64,
                    },
                )
            }))
            .is_err());
            drop(input);
        } else {
            if failure == "cancel" {
                control.cancellation().cancel();
            }
            let mut empty = std::io::empty();
            let reader: &mut dyn Read = if failure == "io" {
                &mut empty
            } else {
                &mut source
            };
            assert!(input
                .advance(
                    reader,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: if failure == "window" { 0 } else { 64 }
                    }
                )
                .is_err());
            drop(input);
        }
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}
