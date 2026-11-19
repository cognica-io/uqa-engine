//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;

fn rejected(bytes: &[u8]) {
    let memory = MemoryBudget::new(16 << 20);
    assert!(FunctionBinding::read_image(&mut &*bytes, &memory, &CancellationToken::new()).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_binding_import_rejects_truncation_trailing_bytes_and_unknown_tags() {
    let bytes = encode(&retained_binding());
    for end in 0..bytes.len() {
        rejected(&bytes[..end]);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    rejected(&trailing);
    let mut version = bytes;
    version[7] = b'2';
    rejected(&version);
    let bare = encode(&binding());
    for count in [0u64, 2, u64::MAX] {
        let mut bad = bare.clone();
        bad[8..16].copy_from_slice(&count.to_le_bytes());
        rejected(&bad);
    }
    for offset in [16, 26, 27, 36, 37, 38, 39] {
        let mut bad = bare.clone();
        bad[offset] = 255;
        rejected(&bad);
    }
    for tail in [
        vec![1, 255, 0],
        vec![1, 0, 12, 0],
        vec![1, 1, 9, 0, 0],
        vec![1, 1, 0, 1, 0, 0, 0, 0, 0],
        vec![1, 2, 2, 0],
        vec![1, 3, 6, 0],
        vec![1, 42, 15, 0, 0, 0],
        vec![1, 42, 0, 6, 0, 0],
        vec![1, 43, 15, 0, 0, 0, 0, 0],
        vec![1, 40, 0, 2, 0],
    ] {
        let mut bad = bare[..38].to_vec();
        bad.extend_from_slice(&tail);
        rejected(&bad);
    }
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    remaining: usize,
    token: &'a CancellationToken,
    unwind: bool,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            assert!(!self.unwind, "injected binding input unwind");
            self.token.cancel();
            return Err(std::io::Error::other("injected binding input cancellation"));
        }
        let n = self.bytes.len().min(out.len()).min(self.remaining);
        out[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        self.remaining -= n;
        Ok(n)
    }
}

#[test]
fn native_binding_import_retires_partial_payloads_after_cancellation_or_unwind() {
    let bytes = encode(&retained_binding());
    for unwind in [false, true] {
        for remaining in [0, 32, bytes.len() / 2, bytes.len() - 1] {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(16 << 20);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                FunctionBinding::read_image(
                    &mut Interrupted {
                        bytes: &bytes,
                        remaining,
                        token: &token,
                        unwind,
                    },
                    &memory,
                    &token,
                )
            }));
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn cancelled_native_binding_readers_are_terminal_and_denied_import_reads_nothing() {
    let value = retained_binding();
    let bytes = encode(&value);
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&token).unwrap();
    assert_eq!(reader.read(&mut [0; 17]).unwrap(), 17);
    token.cancel();
    assert!(reader.read(&mut [0; 17]).is_err());
    assert!(reader.read(&mut [0; 17]).is_err());
    assert!(value.image_reader(&token).is_err());
    let memory = MemoryBudget::new(16 << 20);
    let mut input = bytes.as_slice();
    assert!(FunctionBinding::read_image(&mut input, &memory, &token).is_err());
    assert_eq!(input.len(), bytes.len());
    assert_eq!(memory.used(), 0);
}
