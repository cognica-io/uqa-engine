//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_work::tests::Reader;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_storage::StorageBackendError;

fn input(control: &StorageReadControl) -> ImageDecoder<PathIndex, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move { PathIndex::read_image_from(&mut channel.input()).await },
    )
    .unwrap()
}

#[test]
fn path_input_preserves_independent_long_prefixes_pairs_and_native_charges() {
    let prefix = "한글\0".repeat(4097);
    let labels = [format!("{prefix}a"), format!("{prefix}b")];
    let expected = [
        path(&[], &[(0, u64::MAX)]),
        path(&[&labels[0]], &[(1, 2), (3, 4)]),
        path(&[&labels[1]], &[]),
    ];
    let bytes = frame(&expected);
    let control = StorageReadControl::with_limit(4 << 20);
    let mut decoder = input(&control);
    let mut reader = Reader::new(&bytes);
    let mut calls = 0;
    let mut after_eof = 0;
    let restored = loop {
        let window = ImageDecodeWindow {
            bytes: [1, 13, 4096][calls % 3],
            steps: 1,
        };
        let before = (reader.bytes, reader.calls);
        after_eof += usize::from(reader.eof);
        let result = decoder.advance(&mut reader, window).unwrap();
        assert!(reader.bytes - before.0 <= window.bytes);
        assert!(reader.calls - before.1 <= 1);
        calls += 1;
        assert!(calls < bytes.len() * 4);
        if let Some(index) = result {
            break index;
        }
    };
    assert!(reader.eof && after_eof >= 2);
    assert_eq!(reader.bytes, bytes.len());
    assert_paths(&restored, &expected);
    assert_eq!(encode(&restored), bytes);
    let ordinary = StorageReadControl::with_limit(4 << 20);
    let synchronous = PathIndex::read_image(&mut bytes.as_slice(), &ordinary).unwrap();
    assert_eq!(ordinary.memory().used(), control.memory().used());
    drop(synchronous);
    assert_eq!(ordinary.memory().used(), 0);
    let saved = restored.clone();
    drop(restored);
    control.cancellation().cancel();
    assert_paths(&saved, &expected);
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn path_input_retires_partial_and_post_eof_owners_without_source_replay() {
    let bytes = frame(&[path(&["saved"], &[(1, 2), (3, 4)])]);
    for after_eof in [false, true] {
        for mode in 0..3 {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut decoder = input(&control);
            let mut reader = Reader::new(&bytes);
            loop {
                assert!(decoder
                    .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
                if if after_eof {
                    reader.eof
                } else {
                    reader.bytes >= 35
                } {
                    break;
                }
            }
            let calls = reader.calls;
            if mode == 0 {
                control.cancellation().cancel();
            }
            if mode < 2 {
                assert!(decoder
                    .advance(
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: 1,
                            steps: usize::from(mode != 1)
                        }
                    )
                    .is_err());
                assert_eq!(control.memory().used(), 0);
                assert!(decoder
                    .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            }
            drop(decoder);
            assert_eq!(reader.calls, calls);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn path_input_rejects_ordering_truncation_and_denied_native_admission() {
    let cases = [
        frame(&[path(&["b"], &[]), path(&["a"], &[])]),
        frame(&[path(&["saved"], &[(3, 4), (1, 2)])]),
        frame(&[path(&["saved"], &[(1, 2)])])[..20].to_vec(),
    ];
    for bytes in cases {
        let control = StorageReadControl::with_limit(1 << 20);
        let decoder = input(&control);
        assert!(decoder.read(&mut Reader::new(&bytes)).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let decoder = input(&control);
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    assert!(matches!(
        decoder.read(&mut frame(&[]).as_slice()),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), held.bytes());
    drop(held);
    assert_eq!(control.memory().used(), 0);
}
