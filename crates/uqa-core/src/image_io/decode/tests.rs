//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

mod copying;
mod nested;
mod ownership;

#[test]
fn native_utf8_input_rejects_invalid_prefixes_at_every_read_width() {
    let invalid = [
        vec![0xc0, 0x80],
        vec![0xe0, 0xa0],
        vec![0xe0, 0x9f, 0x80],
        vec![0xed, 0xa0, 0x80],
        vec![0xf4, 0x90, 0x80, 0x80],
        vec![0xf0, 0x90, 0x80],
        vec![0xf0, 0x9f, b'a'],
        vec![0xf5, 0x80, 0x80, 0x80],
        vec![0x80],
    ];
    for payload in invalid {
        assert!(std::str::from_utf8(&payload).is_err());
        for width in [1, 2, 3, 7, 4096] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut source = Cursor::new(text_image(&payload));
            let mut input = text_input(&memory, &token);
            loop {
                match input.advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: width,
                        steps: 3,
                    },
                ) {
                    Ok(None) => (),
                    Err(ImageIoError::Invalid) => break,
                    _ => panic!("invalid UTF-8 was accepted or changed error origin"),
                }
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

fn text_image(value: &[u8]) -> Vec<u8> {
    let mut bytes = (value.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(value);
    bytes
}

fn text_input(memory: &MemoryBudget, token: &CancellationToken) -> ImageDecoder<Budgeted<String>> {
    ImageDecoder::new(memory, token, |mut reader| async move {
        let mut input = reader.input();
        let value = input.text().await?;
        input.end().await?;
        Ok(value)
    })
    .unwrap()
}

#[test]
fn native_decode_windows_preserve_split_utf8_and_original_payload_ownership() {
    let text = "aλ한😀".repeat(2049);
    for width in [1, 3, 17, 4096, 16384] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let bytes = text_image(text.as_bytes());
        let mut reader = Cursor::new(bytes);
        let mut input = text_input(&memory, &token);
        let value = loop {
            let before = reader.position();
            let value = input
                .advance(
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: width,
                        steps: 3,
                    },
                )
                .unwrap();
            assert!(reader.position() - before <= width as u64);
            if let Some(value) = value {
                break value;
            }
        };
        assert_eq!(*value, text);
        assert_eq!(memory.used(), value.reserved_bytes());
        token.cancel();
        assert!(input
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        assert_eq!(*value, text);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_decode_checkpoints_bound_non_io_steps_without_reader_calls() {
    struct NoRead;
    impl Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("checkpoint read source")
        }
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let observed = Arc::new(AtomicUsize::new(0));
    let progress = observed.clone();
    let mut input = ImageDecoder::<()>::new(&memory, &token, |mut reader| async move {
        let mut input = reader.input();
        for _ in 0..103 {
            input.checkpoint().await?;
            progress.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    })
    .unwrap();
    loop {
        let before = observed.load(Ordering::SeqCst);
        let complete = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 7 })
            .unwrap();
        assert!(observed.load(Ordering::SeqCst) - before <= 7);
        if complete.is_some() {
            break;
        }
    }
    assert_eq!(observed.load(Ordering::SeqCst), 103);
    assert_eq!(memory.used(), 0);
}

struct InterruptedReader {
    attempts: usize,
}
impl Read for InterruptedReader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        self.attempts += 1;
        Err(std::io::ErrorKind::Interrupted.into())
    }
}

#[test]
fn interrupted_native_reads_consume_work_and_resume_without_replay() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = text_input(&memory, &token);
    let mut reader = InterruptedReader { attempts: 0 };
    for _ in 0..3 {
        let before = reader.attempts;
        assert!(input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 11,
                    steps: 5
                }
            )
            .unwrap()
            .is_none());
        assert_eq!(reader.attempts - before, 5);
    }
    assert_eq!(
        *input.read(&mut text_image(b"retained").as_slice()).unwrap(),
        "retained"
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn partial_native_decoders_retire_on_quota_cancel_invalid_input_and_unwind() {
    for failure in [
        "zero_bytes",
        "zero_steps",
        "cancel",
        "memory",
        "short",
        "utf8",
        "trailing",
        "unwind",
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut bytes = text_image(if failure == "utf8" {
            &[0xc0, 0x80]
        } else {
            b"original"
        });
        if failure == "short" {
            bytes.pop();
        }
        if failure == "trailing" {
            bytes.push(0);
        }
        let mut reader = Cursor::new(bytes);
        let mut input = text_input(&memory, &token);
        assert!(input
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
            .is_none());
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if failure == "cancel" {
            token.cancel();
        }
        if failure == "unwind" {
            struct Panics;
            impl Read for Panics {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    panic!("native input callback")
                }
            }
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = input.advance(
                    &mut Panics,
                    ImageDecodeWindow {
                        bytes: 17,
                        steps: 5,
                    },
                );
            }))
            .is_err());
        } else {
            let window = ImageDecodeWindow {
                bytes: if failure == "zero_bytes" { 0 } else { 17 },
                steps: if failure == "zero_steps" { 0 } else { 5 },
            };
            loop {
                match input.advance(&mut reader, window) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("accepted {failure}"),
                    Err(_) => break,
                }
            }
        }
        drop(full);
        assert_eq!(memory.used(), 0, "{failure}");
        assert!(input
            .advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 17,
                    steps: 5
                }
            )
            .is_err());
    }
}
