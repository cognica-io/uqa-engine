//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition requirements and mergeable markers use redb's same physical commit boundary.

use super::*;

#[test]
fn revision_guards_preserve_independent_writers_and_closed_file_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("revision-guards.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        uqa_storage::mvcc::verify_revision_guards(&storage.store(), &storage.store()).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    assert_eq!(
        reopened.store().get(b"guard-data").unwrap().as_deref(),
        Some(&b"data-v1"[..])
    );
    assert_eq!(
        reopened
            .store()
            .get(b"guard-definition")
            .unwrap()
            .as_deref(),
        Some(&b"after-undo"[..])
    );
    assert!(reopened.store().get(b"guard-undone-row").unwrap().is_none());
}
