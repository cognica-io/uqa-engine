//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::ImageDecodeWindow;

#[test]
fn table_header_input_preserves_wire_fields_across_current_byte_and_poll_windows() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = memory_table_metadata_input(&control).unwrap();
    let mut reader = std::io::Cursor::new(wire());
    let mut calls = 0;
    let decoded = loop {
        let before = reader.position();
        let bytes = [1, 7, 17][calls % 3];
        calls += 1;
        let result = input
            .advance(&mut reader, ImageDecodeWindow { bytes, steps: 1 })
            .unwrap();
        assert!(reader.position() - before <= bytes as u64);
        if let Some(value) = result {
            break value;
        }
    };
    assert!(calls > 10);
    assert_eq!(decoded.properties, properties());
    assert_eq!(
        &**decoded.fts_fields,
        &["body".to_owned(), "한글".to_owned()]
    );
    assert_eq!(&**decoded.analyzer, &[0, 1, 255]);
    assert_eq!(decoded.row_type_array_name.as_deref(), Some(""));
    drop((decoded, input));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn table_header_input_retires_partial_fields_on_errors_and_abandonment() {
    struct PanicRead;
    impl Read for PanicRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("header source unwound")
        }
    }
    for mode in [
        "cancel",
        "memory",
        "window",
        "drop",
        "unwind",
        "trailing",
        "truncated",
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = memory_table_metadata_input(&control).unwrap();
        let mut bytes = wire();
        if mode == "trailing" {
            bytes.push(0);
        }
        if mode == "truncated" {
            bytes.pop();
        }
        let mut reader = std::io::Cursor::new(bytes);
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        while reader.position() < 90 {
            assert!(input.advance(&mut reader, window).unwrap().is_none());
        }
        if mode == "drop" {
            drop(input);
        } else if mode == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || input.read(&mut PanicRead)
            ))
            .is_err());
        } else {
            let full = if mode == "memory" {
                Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                )
            } else {
                None
            };
            if mode == "cancel" {
                control.cancellation().cancel();
            }
            let window = ImageDecodeWindow {
                bytes: usize::from(mode != "window"),
                ..window
            };
            loop {
                match input.advance(&mut reader, window) {
                    Ok(None) => {}
                    Ok(Some(_)) => panic!("incomplete header published: {mode}"),
                    Err(_) => break,
                }
            }
            assert!(input.advance(&mut reader, window).is_err());
            drop((input, full));
        }
        assert_eq!(control.memory().used(), 0, "{mode}");
    }
}
