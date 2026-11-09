//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::roles::persistence::RoleCatalogSnapshot;
use crate::catalog::sequence::registry::{SequenceCatalog, SequenceRecord};
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
    let mut catalog = SequenceCatalog::with_budget(&MemoryBudget::new(16 << 20));
    for (relation, object_id, persistence, oid, security) in [
        (
            first,
            [2; 16],
            RelationPersistence::Permanent,
            31_001,
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        ),
        (
            second,
            [3; 16],
            RelationPersistence::Unlogged,
            31_002,
            empty_acl,
        ),
    ] {
        catalog
            .insert(
                relation,
                SequenceRecord {
                    state: state(),
                    object_id,
                    persistence,
                    catalog_oid: Some(oid),
                },
                security,
            )
            .unwrap();
    }
    catalog
        .replace_catalog_oids([([2; 16], 31_001), ([3; 16], 31_002), ([9; 16], 31_999)])
        .unwrap();
    SequenceReadSnapshot {
        catalog: uqa_core::memory::RetainedValue::new(catalog),
        roles: RoleCatalogSnapshot {
            roles: uqa_core::memory::RetainedValue::default(),
            memberships: uqa_core::memory::RetainedValue::default(),
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
    uqa_core::memory::RetainedValue::make_mut(&mut source.catalog)
        .insert(
            temp,
            SequenceRecord {
                state: state(),
                object_id: [4; 16],
                persistence: RelationPersistence::Temporary,
                catalog_oid: Some(31_003),
            },
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    // Temporary complete records never enter either durable capture root.
    let captured = durable_sequences(&source, &control).unwrap();
    assert!(captured.iter().eq(original.iter()));
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(captured.len(), 2);
    let rows = captured.iter().collect::<Vec<_>>();
    assert_eq!(
        rows[0].relation,
        &RelationIdentity::new("Quoted.Schema", "Sequence.Name")
    );
    assert_eq!(rows[0].object_id, [2; 16]);
    assert_eq!(rows[0].catalog_oid, Some(31_001));
    assert_eq!(rows[0].state, state());
    assert_eq!(rows[0].security.acl, None);
    assert_eq!(rows[1].persistence, RelationPersistence::Unlogged);
    assert_eq!(rows[1].security.acl, Some(vec![]));
    assert_eq!(rows[1].catalog_oid, Some(31_002));
    drop((source, captured, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_sequence_capture_retains_complete_metadata_after_rejected_publication() {
    let mut source = fixture();
    let original = source.clone();
    let selected = RelationIdentity::new("public", "unlogged");
    let native = source.catalog.budget().clone();
    let used = native.used();
    let occupied = native.reserve(native.available()).unwrap();
    let replacement = SequenceRecord {
        object_id: [4; 16],
        catalog_oid: Some(32_000),
        ..*source.catalog.get(&selected).unwrap()
    };
    assert_eq!(
        uqa_core::memory::RetainedValue::make_mut(&mut source.catalog)
            .insert(
                selected.clone(),
                replacement,
                BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP)
            )
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(std::ptr::eq(
        source.catalog.get(&selected).unwrap(),
        original.catalog.get(&selected).unwrap()
    ));
    assert_eq!(
        source.catalog.security()[&selected],
        original.catalog.security()[&selected]
    );
    assert_eq!(source.catalog.catalog_oids().get(&[4; 16]), None);
    drop(occupied);
    assert_eq!(native.used(), used);
    let control = StorageReadControl::with_limit(4096);
    let captured = source.freeze(&control).unwrap();
    assert_eq!(
        captured.get().unwrap().iter().nth(1).unwrap().object_id,
        [3; 16]
    );
    drop((captured, source, original));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(native.used(), 0);
}

#[test]
fn sequence_capture_preserves_extreme_positions_definition_and_owner_identity() {
    let source = fixture();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let captured = durable_sequences(&source, &control).unwrap();
    assert!(captured.iter().all(|row| row.state == state()));
    drop(source);
    assert_eq!(captured.iter().next().unwrap().state.current, i64::MAX);
    assert!(!captured.iter().next().unwrap().state.called);
    drop(captured);
    assert_eq!(memory.used(), 0);
    let mut source = fixture();
    let catalog = uqa_core::memory::RetainedValue::make_mut(&mut source.catalog);
    catalog
        .insert(
            RelationIdentity::new("s", "x".repeat(32768)),
            SequenceRecord {
                state: SequenceState::initial(1, 1, uqa_sql::ast::SequenceDataType::default()),
                object_id: [4; 16],
                persistence: RelationPersistence::Permanent,
                catalog_oid: None,
            },
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    assert_eq!(
        durable_sequences(&source, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        durable_sequences(&source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
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
