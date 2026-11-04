//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::roles::persistence::RoleCatalogSnapshot;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

fn state() -> SequenceState {
    SequenceState {
        start: i64::MIN,
        increment: -3,
        current: i64::MAX,
        called: false,
        log_count: 31,
        data_type: uqa_sql::ast::SequenceDataType::default(),
        min_value: i64::MIN,
        max_value: i64::MAX,
        cycle: true,
        cache_size: 13,
        definition_generation: [7; 16],
        owner: Some(uqa_storage::SequenceOwner {
            table_object_id: [8; 16],
            column_object_id: [9; 16],
            dependency: uqa_core::catalog_sequence::SequenceOwnerDependency::Internal,
        }),
    }
}

fn fixture() -> SequenceReadSnapshot {
    let first = RelationIdentity::new("Quoted.Schema", "Sequence.Name");
    let second = RelationIdentity::new("public", "unlogged");
    let mut empty_acl = BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP);
    empty_acl.acl = Some(vec![]);
    SequenceReadSnapshot {
        sequences: Arc::new(BTreeMap::from([
            (first.clone(), state()),
            (second.clone(), state()),
        ])),
        object_ids: Arc::new(BTreeMap::from([
            (first.clone(), [2; 16]),
            (second.clone(), [3; 16]),
        ])),
        persistence: Arc::new(BTreeMap::from([
            (first.clone(), RelationPersistence::Permanent),
            (second.clone(), RelationPersistence::Unlogged),
        ])),
        security: Arc::new(BTreeMap::from([
            (first, BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP)),
            (second, empty_acl),
        ])),
        catalog_oids: Arc::new(BTreeMap::from([
            ([2; 16], 31_001),
            ([3; 16], 31_002),
            ([9; 16], 31_999),
        ])),
        roles: RoleCatalogSnapshot {
            roles: Arc::default(),
            memberships: Arc::default(),
        },
    }
}

#[test]
fn durable_sequence_capture_joins_exact_metadata_and_excludes_temporary_capacity() {
    let mut source = fixture();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let original = durable_sequences(&source, &control).unwrap();
    let retained = memory.used();
    let temp = RelationIdentity::new("pg_temp_1", "t".repeat(1 << 20));
    Arc::make_mut(&mut source.sequences).insert(temp.clone(), state());
    Arc::make_mut(&mut source.persistence).insert(temp, RelationPersistence::Temporary);
    // Exclusion precedes both metadata joining and allocation; no private identity/authority is needed.
    let captured = durable_sequences(&source, &control).unwrap();
    assert_eq!(&*captured, &*original);
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].relation,
        RelationIdentity::new("Quoted.Schema", "Sequence.Name")
    );
    assert_eq!(captured[0].object_id, [2; 16]);
    assert_eq!(captured[0].catalog_oid, Some(31_001));
    assert_eq!(captured[0].state, state());
    assert_eq!(captured[0].security.acl, None);
    assert_eq!(captured[1].persistence, RelationPersistence::Unlogged);
    assert_eq!(captured[1].security.acl, Some(vec![]));
    assert_eq!(captured[1].catalog_oid, Some(31_002));
    drop((source, captured, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_sequence_capture_rejects_incomplete_selected_metadata_without_leases() {
    for missing in ["persistence", "identity", "authority"] {
        let mut source = fixture();
        let selected = RelationIdentity::new("public", "unlogged");
        match missing {
            "persistence" => {
                Arc::make_mut(&mut source.persistence).remove(&selected);
            }
            "identity" => {
                Arc::make_mut(&mut source.object_ids).remove(&selected);
            }
            "authority" => {
                Arc::make_mut(&mut source.security).remove(&selected);
            }
            _ => unreachable!(),
        }
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        assert!(matches!(
            durable_sequences(&source, &control),
            Err(SQLError::Internal(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn durable_sequence_capture_admission_and_original_cancellation_are_independent() {
    let source = fixture();
    let control = StorageReadControl::with_limit(4096);
    let frozen = source.freeze(&control).unwrap();
    let retained = control.memory().used();
    for limit in 0..retained {
        let denied = StorageReadControl::with_limit(limit);
        assert!(source.freeze(&denied).is_err(), "{limit}");
        assert_eq!(denied.memory().used(), 0);
    }
    let exact = StorageReadControl::with_limit(retained);
    let independent = source.freeze(&exact).unwrap();
    drop(source);
    control.cancellation().cancel();
    assert!(frozen.get().is_err());
    assert_eq!(independent.get().unwrap().len(), 2);
    drop((independent, frozen));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(exact.memory().used(), 0);
}
