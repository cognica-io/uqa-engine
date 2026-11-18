//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RestoreDestination, SQLiteBackupMapping, SQLiteBackupTarget};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_engine::Engine;
use uqa_storage::StorageEncryptionKey;
use uqa_storage_redb::{RedbBackupTarget, RedbStorage};
use uqa_storage_sqlite::{
    detect_database_file_format, DatabaseFileFormat, ManagedConnection, SQLiteCompressionOptions,
    SQLiteKeyValueStorage, SQLiteStorageProvider,
};

const SOURCE_KEY: &str = "source database key";
const TARGET_KEY: &str = "restored database key";

#[derive(Clone, Copy, Debug)]
pub(super) enum FileMode {
    Plain,
    Encrypted,
    Compressed,
    CompressedEncrypted,
}

impl FileMode {
    fn connect(self, path: &Path, key: &str) -> uqa_storage_sqlite::Result<ManagedConnection> {
        match self {
            Self::Plain => ManagedConnection::open(path),
            Self::Encrypted => ManagedConnection::open_encrypted(path, key),
            Self::Compressed => {
                ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default())
            }
            Self::CompressedEncrypted => ManagedConnection::open_compressed_encrypted(
                path,
                key,
                SQLiteCompressionOptions::default(),
            ),
        }
    }

    fn encrypted(self) -> bool {
        matches!(self, Self::Encrypted | Self::CompressedEncrypted)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Provider {
    Memory,
    Native,
    KeyValue,
    Redb,
}

impl Provider {
    pub(super) fn open(self, path: &Path, mode: FileMode) -> Engine {
        self.open_with_key(path, mode, SOURCE_KEY)
    }

    fn open_with_key(self, path: &Path, mode: FileMode, key: &str) -> Engine {
        match self {
            Self::Memory => Engine::new(),
            Self::Native if matches!(mode, FileMode::Plain) => Engine::open(path).unwrap(),
            Self::KeyValue if matches!(mode, FileMode::Plain) => Engine::from_persistent_provider(
                Arc::new(SQLiteKeyValueStorage::open(path).unwrap()),
            )
            .unwrap(),
            Self::Redb => {
                Engine::from_persistent_provider(Arc::new(RedbStorage::open(path).unwrap()))
                    .unwrap()
            }
            Self::Native => Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(
                mode.connect(path, key).unwrap(),
            )))
            .unwrap(),
            Self::KeyValue => Engine::from_persistent_provider(Arc::new(
                SQLiteKeyValueStorage::from_connection(mode.connect(path, key).unwrap()).unwrap(),
            ))
            .unwrap(),
        }
    }

    pub(super) fn destination(self, directory: PathBuf, mode: FileMode) -> RestoreDestination {
        let mapping = match self {
            Self::Memory => return RestoreDestination::Memory,
            Self::Redb => {
                return RestoreDestination::Persistent {
                    directory,
                    target: Arc::new(RedbBackupTarget::default()),
                }
            }
            Self::Native => SQLiteBackupMapping::Native,
            Self::KeyValue => SQLiteBackupMapping::KeyValue,
        };
        let mut target = SQLiteBackupTarget::new(mapping);
        target.compression = matches!(mode, FileMode::Compressed | FileMode::CompressedEncrypted)
            .then(SQLiteCompressionOptions::default);
        target.encryption_key = mode
            .encrypted()
            .then(|| StorageEncryptionKey::new(TARGET_KEY));
        RestoreDestination::Persistent {
            directory,
            target: Arc::new(target),
        }
    }

    pub(super) fn reopen(self, directory: &Path, mode: FileMode) -> Option<Engine> {
        if matches!(self, Self::Memory) {
            return None;
        }
        let path = directory.join("database");
        if matches!(self, Self::Native | Self::KeyValue) {
            let expected = match mode {
                FileMode::Plain => DatabaseFileFormat::PlainSQLite,
                FileMode::Encrypted => DatabaseFileFormat::Unrecognized,
                FileMode::Compressed | FileMode::CompressedEncrypted => {
                    DatabaseFileFormat::CompressedContainer {
                        encrypted: mode.encrypted(),
                    }
                }
            };
            assert_eq!(detect_database_file_format(&path).unwrap(), expected);
        }
        if mode.encrypted() {
            assert!(mode.connect(&path, SOURCE_KEY).is_err());
        }
        Some(self.open_with_key(&path, mode, TARGET_KEY))
    }
}
