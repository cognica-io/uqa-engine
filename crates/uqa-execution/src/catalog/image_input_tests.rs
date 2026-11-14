//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Cursor, Read};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::SQLError;

pub(super) const WINDOWS: [(usize, usize); 3] = [(1, 7), (17, 1), (4096, 3)];

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
        panic!("native input read failed")
    }
}

pub(super) fn read_windowed<T>(
    mut input: ImageDecoder<T, SQLError>,
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
        let value = input.advance(&mut reader, window).unwrap();
        assert!(reader.input.position() - position <= width as u64);
        assert!(reader.calls - calls <= steps);
        if let Some(value) = value {
            assert_eq!(reader.input.position(), bytes.len() as u64);
            assert!(input.advance(&mut NoRead, window).is_err());
            return value;
        }
    }
    panic!("native image did not complete")
}

pub(super) fn assert_retirement<T>(
    original: &[u8],
    create: impl Fn(
        &MemoryBudget,
        &MemoryBudget,
        &CancellationToken,
    ) -> Result<ImageDecoder<T, SQLError>, SQLError>,
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
        let prefix = if failure == "memory" { 1 } else { 37 };
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
        assert_eq!(memory.used(), 0, "{failure}");
        assert!(input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
    }
}
