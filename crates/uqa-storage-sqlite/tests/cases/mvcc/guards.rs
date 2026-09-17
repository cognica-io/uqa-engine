//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` Key/Value sessions retain read conditions and merge independent marker touches.

use super::*;
use uqa_storage_sqlite::SQLiteKeyValueStore;

#[test]
fn revision_guards_preserve_independent_writers_and_reopen_in_every_file_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("revision-guards.db");
        {
            let a = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
            let b = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
            verify_revision_guards(&a, &b).unwrap();
        }
        let reopened = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
        assert_eq!(
            reopened.get(b"guard-data").unwrap().as_deref(),
            Some(&b"data-v1"[..])
        );
        assert_eq!(
            reopened.get(b"guard-definition").unwrap().as_deref(),
            Some(&b"after-undo"[..])
        );
        assert!(reopened.get(b"guard-undone-row").unwrap().is_none());
    }
}
