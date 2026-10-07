//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact relation identity retention through ordinary row grants.

use super::*;
use crate::row_locks::{LockRequest, RowLockKey, RowLockManager};
use uqa_sql::ast::{LockStrength, LockWait};

mod protocol;

#[test]
fn row_identity_pins_follow_relations_and_savepoint_acquisitions() {
    let directory = tempfile::tempdir().unwrap();
    let manager = RowLockManager::for_database_file(&directory.path().join("rows.db"));
    let coordinator = manager.coordinator().unwrap().unwrap();
    let cancel = CancellationToken::new();
    let name = "public.exact_rows";
    let table = manager.table_key(name);
    let request = |doc_id, strength, mark| LockRequest {
        session_id: 1,
        key: RowLockKey { table, doc_id },
        strength,
        mark,
        wait: LockWait::NoWait,
        cancel: &cancel,
        relation: name,
    };
    let requests = (1..=128)
        .map(|id| request(id, LockStrength::ForKeyShare, 0))
        .collect::<Vec<_>>();
    manager.acquire_batch(&requests).unwrap();
    assert_eq!(coordinator.relation_registry_counts(), (1, 1));
    let original = coordinator
        .retained_row_identity(name.as_bytes(), 1)
        .unwrap();
    manager.acquire_batch(&requests).unwrap();
    manager
        .acquire(&request(1, LockStrength::ForUpdate, 1))
        .unwrap();
    manager.release_mark_above(1, 0);
    assert_eq!(
        coordinator.retained_row_identity(name.as_bytes(), 1),
        Some(original)
    );
    manager.release_session(1);
    assert_eq!(coordinator.relation_registry_counts(), (0, 1));
    assert_eq!(coordinator.retained_row_identity(name.as_bytes(), 1), None);

    let other = coordinator
        .pin_row(b"public.other_rows", 1, &cancel)
        .unwrap();
    assert_ne!(other.identity(), original);
    assert_eq!(coordinator.relation_registry_counts(), (1, 1));
    drop(other);
    assert_eq!(coordinator.relation_registry_counts(), (0, 1));
}

#[test]
fn key_reservations_preserve_every_byte_without_per_key_registry_entries() {
    let directory = tempfile::tempdir().unwrap();
    let manager = RowLockManager::for_database_file(&directory.path().join("keys.db"));
    let coordinator = manager.coordinator().unwrap().unwrap();
    let cancel = CancellationToken::new();
    let requests = (0_u64..128)
        .map(|id| {
            let mut digest = [0; 32];
            digest[24..].copy_from_slice(&id.to_be_bytes());
            LockRequest {
                session_id: 1,
                key: RowLockKey {
                    table: manager.key_reservation_key(digest),
                    doc_id: u64::MAX,
                },
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: LockWait::NoWait,
                cancel: &cancel,
                relation: "keys",
            }
        })
        .collect::<Vec<_>>();
    manager.acquire_batch(&requests).unwrap();
    assert_eq!(coordinator.relation_registry_counts(), (0, 0));
    assert_eq!(manager.state.lock().rows.len(), 128);
    manager.release_session(1);
    assert!(manager.state.lock().rows.is_empty());
    assert_eq!(coordinator.relation_registry_counts(), (0, 0));
}

#[test]
fn exact_row_identity_codec_keeps_namespaces_and_full_document_width() {
    for identity in [
        RowIdentity::Relation {
            generation: i64::MAX as u64,
            doc_id: u64::MAX,
        },
        RowIdentity::Key {
            digest: [0xff; 32],
            doc_id: u64::MAX,
        },
    ] {
        assert_eq!(RowIdentity::decode(&identity.encode()), Some(identity));
        let mut invalid = identity.encode();
        invalid[0] = 3;
        assert_eq!(RowIdentity::decode(&invalid), None);
    }
    assert_eq!(RowIdentity::decode(&[0; 48]), None);
    assert_eq!(RowIdentity::decode(&[]), None);
}
