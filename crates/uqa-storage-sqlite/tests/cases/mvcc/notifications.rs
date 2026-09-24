//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{open, Mode, MODES};
use std::path::Path;
use uqa_storage::{notifications::conformance::*, PersistentStorageProvider};
use uqa_storage_sqlite::{SQLiteKeyValueStorage, SQLiteStorageProvider};

fn provider(mode: Mode, path: &Path, native: bool) -> Box<dyn PersistentStorageProvider> {
    let connection = open(mode, path);
    if native {
        Box::new(SQLiteStorageProvider::new(connection))
    } else {
        Box::new(SQLiteKeyValueStorage::from_connection(connection).unwrap())
    }
}

#[test]
fn native_notification_publication_survives_reopen_in_every_file_mode() {
    verify(true);
}

#[test]
fn key_value_notification_publication_survives_reopen_in_every_file_mode() {
    verify(false);
}

fn verify(native: bool) {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notification-publication.db");
        {
            let first = provider(mode, &path, native);
            let writer = first.open_session().unwrap();
            let second = provider(mode, &path, native);
            let reader = second.open_session().unwrap();
            verify_publication_sessions(&writer, &reader).unwrap();
        }
        if matches!(mode, Mode::Encrypted | Mode::CompressedEncrypted) {
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes
                .windows(b"original notification".len())
                .any(|window| window == b"original notification"));
        }
        {
            let reopened = provider(mode, &path, native);
            verify_publication_reopen(&reopened.open_session().unwrap()).unwrap();
        }
        let reopened = provider(mode, &path, native);
        verify_publication_cleared(&reopened.open_session().unwrap()).unwrap();
    }
}
