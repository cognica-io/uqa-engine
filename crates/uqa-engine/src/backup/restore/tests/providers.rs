//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use std::{path::Path, sync::Arc};
use uqa_storage::PersistentStorageProvider;

pub(super) fn destination(provider: Provider, path: &Path) -> RestoreDestination {
    use uqa_storage_sqlite::{SQLiteBackupMapping, SQLiteBackupTarget};
    let target: Arc<dyn PersistentBackupTarget> = match provider {
        Provider::Memory => return RestoreDestination::Memory,
        Provider::SQLiteNative => Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::Native)),
        Provider::SQLiteKeyValue => {
            Arc::new(SQLiteBackupTarget::new(SQLiteBackupMapping::KeyValue))
        }
        Provider::Redb => Arc::new(uqa_storage_redb::RedbBackupTarget::default()),
    };
    RestoreDestination::Persistent {
        directory: path.to_path_buf(),
        target,
    }
}

pub(super) fn capture_provider(
    provider: &dyn PersistentStorageProvider,
    repository: &LocalRepository,
) -> BackupRoot {
    let control = StorageReadControl::with_limit(32 << 20);
    let image = provider.capture_backup_image(&control).unwrap();
    let mut job = repository
        .begin_image_backup(image, None, 32 << 20, 16 << 20, &control)
        .unwrap();
    while job.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}
