//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor reuse is required even between Weak expiration and the old owner's completed destruction.

use super::*;

#[test]
fn an_expired_registry_reuses_its_still_owned_native_descriptor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("descriptor.db");
    std::fs::write(&path, []).unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let identity = DatabaseId::from_bytes([7; 16]);
    let first = registry(&path, identity).unwrap();
    let original = lease_file(&path, identity).unwrap();
    drop(first);
    let second = registry(&path, identity).unwrap();
    let replacement = lease_file(&path, identity).unwrap();
    assert!(original.shares_descriptor(&replacement));
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = second
        .capture(&control, || Ok(CommitSequence::from_u64(2)))
        .unwrap();
    drop(original);
    assert_eq!(
        second.reclaim(&control, Ok).unwrap(),
        Some(snapshot.sequence())
    );
    drop(snapshot);
    assert_eq!(second.reclaim(&control, Ok).unwrap(), None);
}
