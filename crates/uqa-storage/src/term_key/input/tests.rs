//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

struct NoReads;
impl std::io::Read for NoReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native term validation must not read input")
    }
}

#[test]
fn canonical_term_continuations_preserve_unicode_validation_original_buffers_and_cancellation() {
    let mut scalar = vec![0];
    scalar.extend("한🙂".repeat(2000).bytes());
    let mut raw = vec![1];
    for _ in 0..5000 {
        raw.extend_from_slice(&0x0061u16.to_be_bytes());
    }
    raw.extend_from_slice(&0xd800u16.to_be_bytes());
    let mut paired = vec![1];
    for _ in 0..3000 {
        paired.extend_from_slice(&[0xd8, 0x00, 0xdc, 0x00]);
    }
    for mut bytes in [
        vec![],
        vec![0],
        vec![2],
        vec![0, 0xff],
        vec![0, 0xe1, 0x80],
        vec![1],
        vec![1, 0],
        vec![1, 0xd8, 0],
        scalar.clone(),
        raw,
        paired,
    ] {
        let expected = match bytes.split_first() {
            Some((0, payload)) => std::str::from_utf8(payload).is_ok(),
            Some((1, payload)) if payload.len() % 2 == 0 => char::decode_utf16(
                payload
                    .chunks_exact(2)
                    .map(|p| u16::from_be_bytes([p[0], p[1]])),
            )
            .any(|v| v.is_err()),
            _ => false,
        };
        assert_eq!(TokenTermKey::from_bytes(bytes.clone()).is_ok(), expected);
        bytes.reserve(17);
        let pointer = bytes.as_ptr() as usize;
        let capacity = bytes.capacity();
        let budget = MemoryBudget::new(128 << 10);
        let token = CancellationToken::new();
        let admitted = Budgeted::new(bytes, budget.reserve(capacity).unwrap());
        let mut decoder = ImageDecoder::new(&budget, &token, move |mut reader| async move {
            let (bytes, memory) = admitted.into_parts();
            let key = TokenTermKey::from_bytes_from(bytes, &mut reader.input()).await?;
            Ok::<_, StorageBackendError>(Budgeted::new(key, memory))
        })
        .unwrap();
        let mut calls = 0;
        loop {
            calls += 1;
            match decoder.advance(&mut NoReads, ImageDecodeWindow { bytes: 1, steps: 1 }) {
                Ok(None) => assert!(calls < 100),
                Ok(Some(key)) => {
                    assert!(expected);
                    assert_eq!(key.as_bytes().as_ptr() as usize, pointer);
                    assert_eq!(key.allocated_bytes(), capacity);
                    assert_eq!(budget.used(), capacity);
                    drop(key);
                    break;
                }
                Err(_) => {
                    assert!(!expected);
                    break;
                }
            }
        }
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(128 << 10);
    let token = CancellationToken::new();
    let capacity = scalar.capacity();
    let admitted = Budgeted::new(scalar, budget.reserve(capacity).unwrap());
    let mut decoder = ImageDecoder::new(&budget, &token, move |mut reader| async move {
        let (bytes, memory) = admitted.into_parts();
        let key = TokenTermKey::from_bytes_from(bytes, &mut reader.input()).await?;
        Ok::<_, StorageBackendError>(Budgeted::new(key, memory))
    })
    .unwrap();
    assert!(decoder
        .advance(&mut NoReads, ImageDecodeWindow { bytes: 1, steps: 1 })
        .unwrap()
        .is_none());
    token.cancel();
    assert!(matches!(
        decoder.advance(&mut NoReads, ImageDecodeWindow { bytes: 1, steps: 1 }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}
