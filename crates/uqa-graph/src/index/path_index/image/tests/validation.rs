//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Error, ErrorKind};

#[test]
fn path_image_import_rejects_noncanonical_corrupt_and_incomplete_inputs() {
    let valid = frame(&[path(&["r"], &[(5, 9)])]);
    let mut cases = (0..valid.len())
        .map(|length| valid[..length].to_vec())
        .collect::<Vec<_>>();
    for offset in [0, 7, 32] {
        let mut bytes = valid.clone();
        bytes[offset] = 255;
        cases.push(bytes);
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    cases.push(trailing);
    for offset in [8, 16, 24, 33] {
        let mut bytes = valid.clone();
        bytes[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        cases.push(bytes);
    }
    for paths in [
        vec![path(&["r"], &[(1, 2), (1, 2)])],
        vec![path(&["r"], &[(2, 1), (1, 2)])],
        vec![path(&["r"], &[(1, 2)]), path(&["r"], &[(2, 3)])],
        vec![path(&["z"], &[]), path(&["a"], &[])],
        vec![path(&["a", "b"], &[]), path(&["a"], &[])],
        vec![path(&[""], &[]), path(&[], &[])],
        vec![path(&["prefix", "z"], &[]), path(&["prefix", "a"], &[])],
    ] {
        cases.push(frame(&paths));
    }
    for bytes in cases {
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(PathIndex::read_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let prefix = "long".repeat(16384);
    for (left, right, valid) in [
        (prefix.clone(), format!("{prefix}a"), true),
        (format!("{prefix}b"), format!("{prefix}a"), false),
        (format!("{prefix}a"), prefix.clone(), false),
        (prefix.clone(), prefix.clone(), false),
    ] {
        let bytes = frame(&[path(&[&left], &[]), path(&[&right], &[])]);
        let control = StorageReadControl::with_limit(1 << 20);
        let result = PathIndex::read_image(&mut bytes.as_slice(), &control);
        assert_eq!(result.is_ok(), valid);
        drop(result);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn path_image_import_admits_declared_capacity_before_consuming_payloads() {
    let cases = [
        frame(&[path(&[&"label".repeat(8192)], &[(1, 2)])]),
        frame(&[path(&vec![""; 8192], &[])]),
        frame(&[path(
            &["r"],
            &(0..8192).map(|id| (id, id + 1)).collect::<Vec<_>>(),
        )]),
        frame(
            &(0..8192)
                .map(|id| path(&[&format!("{id:04}")], &[]))
                .collect::<Vec<_>>(),
        ),
    ];
    for bytes in cases {
        let control = StorageReadControl::with_limit(4096);
        let mut input = bytes.as_slice();
        let result = PathIndex::read_image(&mut input, &control);
        assert!(matches!(result, Err(StorageBackendError::Memory(_))));
        assert!(bytes.len() - input.len() < 64);
        assert_eq!(control.memory().used(), 0);
    }
}

struct PartialInput<'a> {
    bytes: &'a [u8],
    attempts: usize,
    cancel: Option<&'a StorageReadControl>,
}

impl Read for PartialInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(output.len() <= 4096);
        self.attempts += 1;
        if self.attempts == 10 {
            if let Some(control) = self.cancel {
                control.cancellation().cancel();
                return Err(ErrorKind::Interrupted.into());
            }
        }
        if self.attempts % 2 == 1 {
            return Err(ErrorKind::Interrupted.into());
        }
        let count = self.bytes.len().min(output.len()).min(3);
        output[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

struct PartialOutput {
    bytes: Vec<u8>,
    attempts: usize,
    fail: u8,
}
impl Write for PartialOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(bytes.len() <= 4096);
        self.attempts += 1;
        if self.attempts % 2 == 1 {
            return Err(ErrorKind::Interrupted.into());
        }
        match self.fail {
            1 => return Ok(0),
            2 => return Err(Error::other("injected output failure")),
            3 => return Ok(bytes.len() + 1),
            _ => {}
        }
        let count = bytes.len().min(3);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn path_images_handle_partial_interrupted_io_and_cancelled_imports() {
    let expected = [path(&["a", "b"], &[(0, 1), (2, u64::MAX)])];
    let bytes = frame(&expected);
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = PartialInput {
            bytes: &bytes,
            attempts: 0,
            cancel: cancel.then_some(&control),
        };
        let result = PathIndex::read_image(&mut input, &control);
        if cancel {
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
            assert_eq!(input.attempts, 10);
        } else {
            assert_paths(&result.unwrap(), &expected);
        }
        assert_eq!(control.memory().used(), 0);
    }
    let source = materialized(&expected);
    for fail in 0..4 {
        let control = StorageReadControl::with_limit(4096);
        let mut output = PartialOutput {
            bytes: Vec::new(),
            attempts: 0,
            fail,
        };
        let result = source.write_image(&mut output, &control);
        if fail == 0 {
            assert_eq!(result.unwrap(), bytes.len() as u64);
            assert_eq!(output.bytes, bytes);
        } else {
            assert!(result.is_err());
            assert_eq!(output.attempts, 2);
        }
        assert_eq!(control.memory().used(), 0);
    }
}
