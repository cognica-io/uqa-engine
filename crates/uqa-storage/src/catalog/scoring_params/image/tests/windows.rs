//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::image_io::ImageDecodeWindow;

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
struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("scoring input read failed")
    }
}

#[test]
fn scoring_input_bounds_opaque_fields_and_preserves_native_mutation_ownership() {
    let name = format!("signal{}", "한🙂".repeat(1025));
    let value = "opaque\0한🙂".repeat(1025);
    let expected = wire(&[("", ""), (&name, &value)]);
    for (width, steps) in [(1, 7), (17, 1), (4096, 3)] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let mut input = ScoringParameters::image_input(&control, &mutations).unwrap();
        let mut reader = Counted {
            input: Cursor::new(expected.as_slice()),
            calls: 0,
        };
        let window = ImageDecodeWindow {
            bytes: width,
            steps,
        };
        let mut restored = loop {
            let position = reader.input.position();
            let calls = reader.calls;
            let value = input.advance(&mut reader, window).unwrap();
            assert!(reader.input.position() - position <= width as u64);
            assert!(reader.calls - calls <= steps);
            if let Some(value) = value {
                break value;
            }
        };
        assert_eq!(reader.input.position(), expected.len() as u64);
        assert!(input.advance(&mut NoRead, window).is_err());
        assert_eq!(restored.get(&name), Some(&value));
        let retained = restored.clone();
        let pointer = retained.get(&name).unwrap().as_ptr();
        let full = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        restored.insert(name.clone(), "changed".into()).unwrap();
        assert_eq!(retained.get(&name).unwrap().as_ptr(), pointer);
        let output = StorageReadControl::with_limit(1 << 20);
        assert_eq!(bytes(&retained.capture(&output).unwrap()), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(control.memory().used() > 0);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn scoring_input_retires_partial_fields_on_every_terminal_path() {
    let value = "한🙂".repeat(2049);
    let original = wire(&[("signal", &value), ("z", "")]);
    for failure in [
        "bytes", "steps", "cancel", "memory", "short", "trailing", "unwind", "drop",
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = ScoringParameters::image_input(&control, control.memory()).unwrap();
        let mut bytes = original.clone();
        if failure == "short" {
            bytes.pop();
        } else if failure == "trailing" {
            bytes.push(0);
        }
        let mut reader = Cursor::new(bytes);
        let prefix = if failure == "memory" { 1 } else { 43 };
        assert!(input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: prefix,
                    steps: usize::MAX
                }
            )
            .unwrap()
            .is_none());
        assert_eq!(reader.position(), prefix as u64);
        assert!(control.memory().used() > 0);
        if failure == "drop" {
            drop(input);
            assert_eq!(control.memory().used(), 0);
            continue;
        }
        let full = (failure == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if failure == "cancel" {
            control.cancellation().cancel();
        }
        let window = ImageDecodeWindow {
            bytes: if failure == "bytes" { 0 } else { 17 },
            steps: if failure == "steps" { 0 } else { usize::MAX },
        };
        if failure == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = input.advance(&mut NoRead, window);
            }))
            .is_err());
        } else {
            let mut rejected = false;
            for _ in 0..original.len() * 8 + 1024 {
                match input.advance(&mut reader, window) {
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
        assert_eq!(control.memory().used(), 0, "{failure}");
        assert!(input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
    }
}
