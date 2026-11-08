//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    backup::{ImageFormat, ImageProvider, SnapshotId},
    image_io::{ImagePreparation, ImageRead},
    mvcc::DatabaseId,
    StorageBackendResult,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Codec {
    preparation: usize,
    offset: usize,
    dropped: Arc<AtomicUsize>,
    panic_read: bool,
}

impl Drop for Codec {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl ImageRead for Codec {
    fn prepare(&mut self, maximum: usize) -> StorageBackendResult<ImagePreparation> {
        let positions = maximum.min(self.preparation);
        self.preparation -= positions;
        Ok(ImagePreparation {
            positions,
            ready: self.preparation == 0,
        })
    }
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        assert_eq!(self.preparation, 0);
        assert!(!self.panic_read, "codec read failure");
        let bytes = b"native evaluated component";
        let count = output.len().min(bytes.len() - self.offset);
        output[..count].copy_from_slice(&bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

struct Entries<'a> {
    keys: std::iter::Copied<std::slice::Iter<'a, Option<u8>>>,
    invalidated: bool,
    invalidate_completion: bool,
    dropped: Arc<AtomicUsize>,
    codecs: Arc<AtomicUsize>,
    panic_read: bool,
}

impl Drop for Entries<'_> {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl NativeImageEntries for Entries<'_> {
    fn check(&self) -> BackupResult<()> {
        if self.invalidated {
            Err(BackupError::Invalid("retained source failed"))
        } else {
            Ok(())
        }
    }

    fn next_image(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep> {
        let Some(key) = self.keys.next() else {
            self.invalidated = self.invalidate_completion;
            return Ok(NativeImageStep::Complete);
        };
        let Some(key) = key else {
            return Ok(NativeImageStep::Pending);
        };
        let mut owned_key = BudgetedVec::new(control.memory());
        owned_key.push(key)?;
        Ok(NativeImageStep::Unit(NativeImageUnit {
            namespace: NamespaceId::from_bytes([3; 16])?,
            generation: 0,
            family: 1,
            key: owned_key,
            kind: UnitKind::Record,
            reader: ImageReader::new(
                Codec {
                    preparation: 3,
                    offset: 0,
                    dropped: self.codecs.clone(),
                    panic_read: self.panic_read,
                },
                control,
            )?,
        }))
    }
}

fn image<'a>(
    keys: &'a [Option<u8>],
    control: &StorageReadControl,
    panic_read: bool,
) -> (
    NativeBackupImage<Entries<'a>>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let dropped = Arc::new(AtomicUsize::new(0));
    let codecs = Arc::new(AtomicUsize::new(0));
    let source = NativeBackupImage::new(
        BackupCut {
            source: DatabaseId::from_bytes([1; 16]),
            snapshot: SnapshotId::from_bytes([2; 16]).unwrap(),
            format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
            sequence: None,
        },
        Entries {
            keys: keys.iter().copied(),
            invalidated: false,
            invalidate_completion: false,
            dropped: dropped.clone(),
            codecs: codecs.clone(),
            panic_read,
        },
        control,
    )
    .unwrap();
    (source, dropped, codecs)
}

fn prepare(image: &mut NativeBackupImage<Entries<'_>>, window: usize) -> BackupSourceStep {
    loop {
        let before = image.statistics().visited_positions;
        let step = image.advance(window).unwrap();
        assert!(image.statistics().visited_positions - before <= window as u64);
        if step != BackupSourceStep::Pending {
            return step;
        }
        assert!(image.current_unit().is_none());
    }
}

fn drain(image: &mut NativeBackupImage<Entries<'_>>, window: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0; window];
    loop {
        let count = image.read_current(&mut buffer).unwrap();
        if count == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn native_image_preparation_and_output_are_bounded_and_preserve_exact_units() {
    for positions in [1, 2, 128] {
        for window in [1, 7, 32768] {
            let control = StorageReadControl::with_limit(4096);
            let (mut source, dropped, codecs) =
                image(&[None, Some(1), None, Some(4)], &control, false);
            let original = source.cut();
            for key in [1, 4] {
                assert_eq!(prepare(&mut source, positions), BackupSourceStep::Unit);
                let unit = source.current_unit().unwrap();
                assert_eq!(unit.address.key, &[key]);
                assert_eq!(unit.decoded_bytes, None);
                assert_eq!(source.read_current(&mut []).unwrap(), 0);
                assert_eq!(drain(&mut source, window), b"native evaluated component");
            }
            assert_eq!(codecs.load(Ordering::SeqCst), 2);
            assert_eq!(prepare(&mut source, positions), BackupSourceStep::Complete);
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(source.cut(), original);
            assert_eq!(source.statistics().visited_positions, 11);
            assert_eq!(source.statistics().copied_payload_bytes, 52);
            assert_eq!(source.statistics().metadata_read_bytes, 0);
            control.cancellation().cancel();
            assert_eq!(source.advance(1).unwrap(), BackupSourceStep::Complete);
        }
    }
}

#[test]
fn native_image_invalid_order_and_unread_advance_retire_the_cut() {
    for keys in [[Some(2), Some(2)], [Some(2), Some(1)]] {
        let control = StorageReadControl::with_limit(4096);
        let (mut source, dropped, codecs) = image(&keys, &control, false);
        prepare(&mut source, 128);
        drain(&mut source, 7);
        assert!(matches!(source.advance(128), Err(BackupError::Invalid(_))));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(codecs.load(Ordering::SeqCst), 2);
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(source.advance(1), Err(BackupError::Finished)));
    }
    let control = StorageReadControl::with_limit(4096);
    let (mut source, dropped, codecs) = image(&[Some(1)], &control, false);
    prepare(&mut source, 128);
    assert_eq!(source.read_current(&mut [0; 1]).unwrap(), 1);
    assert!(matches!(source.advance(1), Err(BackupError::Invalid(_))));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(codecs.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_image_cancellation_admission_and_unwind_cannot_resume() {
    for after_prepare in [false, true] {
        let control = StorageReadControl::with_limit(4096);
        let (mut source, dropped, codecs) = image(&[Some(1)], &control, false);
        if after_prepare {
            prepare(&mut source, 128);
        } else {
            assert_eq!(source.advance(1).unwrap(), BackupSourceStep::Pending);
        }
        control.cancellation().cancel();
        assert!(source.advance(1).is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(codecs.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(
            source.read_current(&mut [0; 1]),
            Err(BackupError::Finished)
        ));
    }
    let denied = StorageReadControl::with_limit(0);
    let (mut source, dropped, _) = image(&[Some(1)], &denied, false);
    assert!(matches!(source.advance(1), Err(BackupError::Memory(_))));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(denied.memory().used(), 0);

    let control = StorageReadControl::with_limit(4096);
    let (mut source, dropped, codecs) = image(&[Some(1)], &control, true);
    prepare(&mut source, 128);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.read_current(&mut [0; 1]).unwrap();
    }))
    .is_err());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(codecs.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(source.advance(1), Err(BackupError::Finished)));
}

#[test]
fn native_image_source_failure_and_invalid_calls_release_all_retained_state() {
    for action in 0..4 {
        let control = StorageReadControl::with_limit(4096);
        let (mut source, dropped, codecs) = image(&[Some(1)], &control, false);
        if action == 0 {
            assert!(matches!(source.advance(0), Err(BackupError::Invalid(_))));
        } else {
            source.advance(1).unwrap();
            if action == 1 {
                assert!(matches!(
                    source.read_current(&mut [0; 1]),
                    Err(BackupError::Invalid(_))
                ));
            } else if action == 2 {
                prepare(&mut source, 128);
                let State::Active(active) = &mut source.state else {
                    unreachable!()
                };
                active.entries.invalidated = true;
                assert!(matches!(
                    source.read_current(&mut [0; 1]),
                    Err(BackupError::Invalid(_))
                ));
                assert_eq!(source.statistics().copied_payload_bytes, 0);
            } else {
                drop(source);
                assert_eq!(dropped.load(Ordering::SeqCst), 1);
                assert_eq!(codecs.load(Ordering::SeqCst), 1);
                assert_eq!(control.memory().used(), 0);
                continue;
            }
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(codecs.load(Ordering::SeqCst), usize::from(action != 0));
        assert_eq!(control.memory().used(), 0);
        assert!(source.current_unit().is_none());
        assert!(matches!(source.advance(1), Err(BackupError::Finished)));
    }
    let control = StorageReadControl::with_limit(4096);
    let (mut source, dropped, codecs) = image(&[Some(1)], &control, false);
    prepare(&mut source, 128);
    drain(&mut source, 7);
    let State::Active(active) = &mut source.state else {
        unreachable!()
    };
    active.entries.invalidate_completion = true;
    assert!(matches!(source.advance(1), Err(BackupError::Invalid(_))));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(codecs.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(source.advance(1), Err(BackupError::Finished)));
}
