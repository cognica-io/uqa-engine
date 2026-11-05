//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::backup::{BackupError, ImageFormat, ImageProvider, SnapshotId};
use crate::mvcc::DatabaseId;
use crate::{PersistentStorageProvider, PersistentStorageSession, StorageBackendResult};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Source {
    control: StorageReadControl,
    dropped: Arc<AtomicUsize>,
}

impl Drop for Source {
    fn drop(&mut self) {
        // The concrete source still owns its entire header allowance during destruction.
        assert_eq!(self.control.memory().used(), size_of::<Self>());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl BackupImageSource for Source {
    fn cut(&self) -> BackupCut {
        BackupCut {
            source: DatabaseId::from_bytes([1; 16]),
            snapshot: SnapshotId::from_bytes([2; 16]).unwrap(),
            format: ImageFormat::new(ImageProvider::Redb, 1).unwrap(),
            sequence: None,
        }
    }
    fn advance(&mut self, _: usize) -> BackupResult<BackupSourceStep> {
        self.control.check()?;
        Ok(BackupSourceStep::Complete)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        None
    }
    fn read_current(&mut self, _: &mut [u8]) -> BackupResult<usize> {
        self.control.check()?;
        Err(BackupError::Finished)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        BackupSourceStatistics::default()
    }
}

#[test]
fn owned_provider_image_admits_before_capture_and_retires_with_original_allowance() {
    let denied = StorageReadControl::with_limit(size_of::<Source>() - 1);
    assert!(matches!(
        OwnedBackupImage::capture::<Source>(&denied, || panic!("unadmitted capture")),
        Err(BackupError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(size_of::<Source>());
    cancelled.cancellation().cancel();
    assert!(matches!(
        OwnedBackupImage::capture::<Source>(&cancelled, || panic!("cancelled capture")),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);

    for cancel_during_capture in [false, true] {
        let control = StorageReadControl::with_limit(size_of::<Source>());
        let dropped = Arc::new(AtomicUsize::new(0));
        let image = OwnedBackupImage::capture(&control, || {
            assert_eq!(control.memory().used(), size_of::<Source>());
            let source = Source {
                control: control.clone(),
                dropped: dropped.clone(),
            };
            if cancel_during_capture {
                control.cancellation().cancel();
            }
            Ok(source)
        });
        if cancel_during_capture {
            assert!(matches!(image, Err(BackupError::Cancelled(_))));
        } else {
            let mut image = image.unwrap();
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            control.cancellation().cancel();
            std::thread::spawn(move || {
                assert!(matches!(image.advance(1), Err(BackupError::Cancelled(_))));
                assert!(matches!(
                    image.read_current(&mut [0; 1]),
                    Err(BackupError::Cancelled(_))
                ));
            })
            .join()
            .unwrap();
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn provider_without_image_mapping_rejects_capture_without_opening_a_session() {
    struct Provider;
    impl PersistentStorageProvider for Provider {
        fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
            panic!("image support cannot be inferred from an ordinary session")
        }
    }
    let provider: &dyn PersistentStorageProvider = &Provider;
    assert_eq!(
        provider.backup_capabilities(),
        crate::backup::BackupCapabilities::UNSUPPORTED
    );
    let control = StorageReadControl::with_limit(1024);
    assert!(matches!(
        provider.capture_backup_image(&control),
        Err(BackupError::Unsupported(_))
    ));
    control.cancellation().cancel();
    assert!(matches!(
        provider.capture_backup_image(&control),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
