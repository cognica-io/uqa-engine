//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

fn decoder(memory: &MemoryBudget, token: &CancellationToken) -> ImageDecoder<Budgeted<Vec<u8>>> {
    ImageDecoder::new(memory, token, |mut reader| async move {
        let mut input = reader.input();
        let value = read_framed_image_from(&mut input, async |mut child| {
            let value = child.bytes().await?;
            child.end().await?;
            Ok::<_, ImageIoError>(value)
        })
        .await?;
        assert_eq!(input.array::<4>().await?, *b"tail");
        input.end().await?;
        Ok(value)
    })
    .unwrap()
}

fn bytes(payload: &[u8]) -> Vec<u8> {
    let mut child = (payload.len() as u64).to_le_bytes().to_vec();
    child.extend_from_slice(payload);
    let mut bytes = encode(&child, 37);
    bytes.extend_from_slice(b"tail");
    bytes
}

#[test]
fn native_framed_children_preserve_canonical_boundaries_and_parent_byte_windows() {
    for length in [0, 1, 4088, 4089, 16384] {
        let expected = vec![31; length];
        let bytes = bytes(&expected);
        for width in [1, 17, 4096] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = decoder(&memory, &token);
            let mut source = Cursor::new(bytes.as_slice());
            let value = loop {
                let before = source.position();
                let value = input
                    .advance(
                        &mut source,
                        ImageDecodeWindow {
                            bytes: width,
                            steps: 3,
                        },
                    )
                    .unwrap();
                assert!(source.position() - before <= width as u64);
                if let Some(value) = value {
                    break value;
                }
            };
            assert_eq!(*value, expected);
            assert_eq!(source.position(), bytes.len() as u64);
            assert_eq!(memory.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_framed_children_reject_truncation_noncanonical_frames_and_unconsumed_bytes() {
    let valid = bytes(b"payload");
    for end in 0..valid.len() - 4 {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(&memory, &token);
        let mut source = &valid[..end];
        loop {
            match input.advance(&mut source, ImageDecodeWindow { bytes: 1, steps: 3 }) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("truncated child accepted"),
                Err(_) => break,
            }
        }
        assert_eq!(memory.used(), 0);
        assert!(input
            .advance(&mut &valid[..], ImageDecodeWindow { bytes: 1, steps: 3 })
            .is_err());
    }
    for bytes in [
        vec![1, 0, 0, 0, 9, 1, 0, 0, 0, 8, 0, 0, 0, 0],
        vec![1, 16, 0, 0],
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let input = ImageDecoder::<()>::new(&memory, &token, |mut reader| async move {
            let mut input = reader.input();
            read_framed_image_from(&mut input, async |mut child| {
                let mut bytes = [0; 4096];
                while child.read(&mut bytes).await? != 0 {}
                Ok::<_, ImageIoError>(())
            })
            .await
        })
        .unwrap();
        assert!(input.read(&mut bytes.as_slice()).is_err());
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let input = ImageDecoder::<()>::new(&memory, &token, |mut reader| async move {
        read_framed_image_from(&mut reader.input(), async |_| Ok::<_, ImageIoError>(())).await
    })
    .unwrap();
    assert!(input.read(&mut encode(b"unused", 3).as_slice()).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_framed_children_resume_interrupts_and_retire_failed_owners() {
    struct Interrupted<'a> {
        source: &'a [u8],
        attempts: usize,
    }
    impl Read for Interrupted<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.attempts += 1;
            if self.attempts.is_multiple_of(3) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let limit = output.len().min(1);
            self.source.read(&mut output[..limit])
        }
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let bytes = bytes(b"original");
    let mut source = Interrupted {
        source: &bytes,
        attempts: 0,
    };
    let mut input = decoder(&memory, &token);
    loop {
        let before = source.attempts;
        let value = input
            .advance(&mut source, ImageDecodeWindow { bytes: 3, steps: 3 })
            .unwrap();
        assert!(source.attempts - before <= 3);
        if let Some(value) = value {
            assert_eq!(&**value, b"original");
            break;
        }
    }
    assert_eq!(memory.used(), 0);
    let mut input = decoder(&memory, &token);
    assert!(input
        .advance(
            &mut bytes.as_slice(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    token.cancel();
    assert!(input
        .advance(
            &mut bytes.as_slice(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .is_err());
    assert_eq!(memory.used(), 0);
}
