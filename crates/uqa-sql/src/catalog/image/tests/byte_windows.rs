//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::ImageDecodeWindow;

mod security;

pub(in crate::catalog::image) const WINDOWS: [(usize, usize); 3] = [(1, 7), (17, 1), (4096, 3)];

struct Counted<'a> {
    input: Cursor<&'a [u8]>,
    calls: usize,
}
impl Read for Counted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        self.input.read(output)
    }
}

pub(in crate::catalog::image) fn read_windowed<T>(
    mut input: CatalogImageInput<T>,
    bytes: &[u8],
    width: usize,
    steps: usize,
) -> T {
    let window = ImageDecodeWindow {
        bytes: width,
        steps,
    };
    let mut reader = Counted {
        input: Cursor::new(bytes),
        calls: 0,
    };
    for _ in 0..bytes.len() * 8 + 1024 {
        let position = reader.input.position();
        let calls = reader.calls;
        let value = input.advance_with_window(&mut reader, 1, window).unwrap();
        assert!(reader.input.position() - position <= width as u64);
        assert!(reader.calls - calls <= steps);
        if let Some(value) = value {
            assert_eq!(reader.input.position(), bytes.len() as u64);
            let calls = reader.calls;
            assert!(input.advance_with_window(&mut reader, 1, window).is_err());
            assert_eq!(reader.calls, calls);
            return value;
        }
    }
    panic!("native catalog input did not complete");
}

pub(in crate::catalog::image) fn assert_retirement<T>(
    original: &[u8],
    create: impl Fn(
        &MemoryBudget,
        &MemoryBudget,
        &CancellationToken,
    ) -> Result<CatalogImageInput<T>, SQLError>,
) {
    for failure in [
        "bytes", "steps", "cancel", "memory", "short", "trailing", "unwind", "drop",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = create(&memory, &memory, &token).unwrap();
        let mut bytes = original.to_vec();
        if failure == "short" {
            bytes.pop();
        } else if failure == "trailing" {
            bytes.push(0);
        }
        let mut reader = Cursor::new(bytes);
        assert!(input
            .advance_with_window(
                &mut reader,
                1,
                ImageDecodeWindow {
                    bytes: 1,
                    steps: 64
                }
            )
            .unwrap()
            .is_none());
        assert_eq!(reader.position(), 1);
        assert!(memory.used() > 0);
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
            steps: if failure == "steps" { 0 } else { usize::MAX },
        };
        if failure == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = input.advance_with_window(&mut PanickingInput, 1, window);
            }))
            .is_err());
        } else {
            let mut rejected = false;
            for _ in 0..original.len() * 8 + 1024 {
                match input.advance_with_window(&mut reader, 1, window) {
                    Err(_) => {
                        rejected = true;
                        break;
                    }
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("{failure} accepted invalid input"),
                }
            }
            assert!(rejected, "{failure} did not retire");
        }
        drop(full);
        assert_eq!(memory.used(), 0, "{failure}");
        let position = reader.position();
        assert!(input.advance(&mut reader, 1).is_err());
        assert_eq!(reader.position(), position);
    }
}

#[test]
fn role_input_bounds_bytes_and_polls_inside_large_utf8_fields() {
    let mut source = catalog();
    let name = format!("z{}", "한🙂".repeat(1025));
    source.insert(name.clone(), role(&name, 101)).unwrap();
    let bytes = encode_roles(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(
            RoleDefinitions::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode_roles(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn schema_input_bounds_bytes_and_polls_through_native_acl_fields() {
    let source = schema_registry();
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let captured = source
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let bytes = streamed(captured.image_reader(&token).unwrap(), 7);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            SchemaRegistry::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        let captured = restored
            .capture(&ProductionControl::new(&memory, &token, &token))
            .unwrap();
        assert_eq!(streamed(captured.image_reader(&token).unwrap(), 7), bytes);
        drop((captured, restored));
        assert_eq!(memory.used(), 0);
    }
}

struct PanickingInput;
impl Read for PanickingInput {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("input failed inside a native field")
    }
}

#[test]
fn catalog_byte_input_retires_partial_fields_on_every_terminal_path() {
    let mut source = RoleDefinitions::new();
    let name = "a".repeat(8193);
    source.insert(name.clone(), role(&name, 101)).unwrap();
    for failure in [
        "entries", "bytes", "steps", "cancel", "memory", "short", "trailing", "unwind",
    ] {
        let mut bytes = encode_roles(&source);
        if failure == "short" {
            bytes.truncate(100);
        } else if failure == "trailing" {
            bytes.push(0);
        }
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = RoleDefinitions::image_input(&memory, &memory, &token).unwrap();
        let mut reader = Cursor::new(bytes);
        assert!(input
            .advance_with_window(
                &mut reader,
                1,
                ImageDecodeWindow {
                    bytes: 37,
                    steps: 64
                }
            )
            .unwrap()
            .is_none());
        assert_eq!(reader.position(), 37);
        assert!(memory.used() > 0);
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if failure == "cancel" {
            token.cancel();
        }
        let window = ImageDecodeWindow {
            bytes: if failure == "bytes" { 0 } else { 17 },
            steps: if failure == "steps" { 0 } else { 7 },
        };
        if failure == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = input.advance_with_window(&mut PanickingInput, 1, window);
            }))
            .is_err());
        } else {
            loop {
                match input.advance_with_window(
                    &mut reader,
                    usize::from(failure != "entries"),
                    window,
                ) {
                    Err(_) => break,
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("{failure} accepted invalid input"),
                }
            }
        }
        drop(full);
        assert_eq!(memory.used(), 0, "{failure}");
        let position = reader.position();
        assert!(input.advance(&mut reader, 1).is_err());
        assert_eq!(reader.position(), position);
    }
}

#[test]
fn catalog_entry_and_byte_windows_share_one_retained_stream_position() {
    let expected = encode_roles(&catalog());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = RoleDefinitions::image_input(&memory, &memory, &token).unwrap();
    let mut reader = Cursor::new(expected.as_slice());
    assert!(input
        .advance_with_window(
            &mut reader,
            1,
            ImageDecodeWindow {
                bytes: 27,
                steps: 64
            }
        )
        .unwrap()
        .is_none());
    assert_eq!(reader.position(), 27);
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    let mut first = RoleDefinitions::new();
    first.insert("alpha".into(), role("alpha", 42)).unwrap();
    assert_eq!(reader.position(), encode_roles(&first).len() as u64);
    let restored = input.advance(&mut reader, 1).unwrap().unwrap();
    assert_eq!(encode_roles(&restored), expected);
    drop(restored);
    assert_eq!(memory.used(), 0);
}
