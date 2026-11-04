//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{security::roles::persistence::RoleCatalogSnapshot, sequence::SequenceState};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::security::BoundSequenceSecurity,
};
use uqa_storage::memory_capture::MemoryCaptureRegistry;

fn fixture() -> SequenceCaptureSource {
    let relation = RelationIdentity::new("Quoted.Schema", "Saved");
    let mut state = SequenceState::initial(41, -3, SequenceDataType::BigInt);
    state.definition_generation = [7; 16];
    state.cache_size = 8;
    let definitions = SequenceReadSnapshot {
        sequences: Arc::new(BTreeMap::from([(relation.clone(), state)])),
        object_ids: Arc::new(BTreeMap::from([(relation.clone(), [1; 16])])),
        persistence: Arc::new(BTreeMap::from([(
            relation.clone(),
            RelationPersistence::Permanent,
        )])),
        security: Arc::new(BTreeMap::from([(
            relation,
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )])),
        catalog_oids: Arc::new(BTreeMap::from([([1; 16], 32_001)])),
        roles: RoleCatalogSnapshot {
            roles: Arc::default(),
            memberships: Arc::default(),
        },
    };
    SequenceCaptureSource::new(definitions)
}

fn value(object_id: [u8; 16], current: i64, called: bool) -> NontransactionalSequenceValue {
    NontransactionalSequenceValue {
        object_id,
        current,
        called,
        log_count: 19,
        autonomous: false,
    }
}

#[test]
fn sequence_value_capture_joins_original_object_and_definition_before_admission() {
    let mut source = fixture();
    source.record([7; 16], value([1; 16], i64::MIN, false), None, false);
    let control = StorageReadControl::with_limit(4096);
    let mut saved = None;
    let original_allocations =
        allocation_counter::measure(|| saved = Some(source.freeze(&control).unwrap()));
    let saved = saved.unwrap();
    let mut expected = source
        .definitions
        .as_ref()
        .unwrap()
        .sequences
        .values()
        .next()
        .copied()
        .unwrap();
    expected.current = i64::MIN;
    expected.called = false;
    expected.log_count = 19;
    assert_eq!(saved.get().unwrap()[0].state, expected);
    assert_eq!(saved.get().unwrap()[0].object_id, [1; 16]);
    assert_eq!(saved.get().unwrap()[0].catalog_oid, Some(32_001));
    // A replaced definition and an unrelated object cannot update the selected original generation.
    source.record([8; 16], value([1; 16], 999, true), None, true);
    source.record([7; 16], value([2; 16], 777, true), None, true);
    let temporary = RelationIdentity::new("pg_temp_1", "x".repeat(1 << 20));
    let definitions = source.definitions.as_mut().unwrap();
    Arc::make_mut(&mut definitions.sequences).insert(temporary.clone(), expected);
    Arc::make_mut(&mut definitions.persistence).insert(temporary, RelationPersistence::Temporary);
    for object in 3_u128..1_003 {
        source.record([7; 16], value(object.to_le_bytes(), 100, true), None, false);
    }
    let second = StorageReadControl::with_limit(4096);
    let mut other = None;
    let with_excluded =
        allocation_counter::measure(|| other = Some(source.freeze(&second).unwrap()));
    assert_eq!(original_allocations.bytes_total, with_excluded.bytes_total);
    assert_eq!(original_allocations.count_total, with_excluded.count_total);
    assert_eq!(control.memory().used(), second.memory().used());
    assert_eq!(other.as_ref().unwrap().get().unwrap()[0].state, expected);
    let required = control.memory().used();
    drop((saved, other));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(second.memory().used(), 0);
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(source.freeze(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let exact = StorageReadControl::with_limit(required);
    let retained = source.freeze(&exact).unwrap();
    assert_eq!(retained.get().unwrap()[0].state, expected);
    drop(source);
    assert_eq!(retained.get().unwrap()[0].state, expected);
    drop(retained);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn sequence_history_publication_preserves_unread_cuts_and_native_session_observations() {
    let registry = MemoryCaptureRegistry::new();
    let source = fixture();
    let weak = Arc::downgrade(&source.definitions.as_ref().unwrap().sequences);
    let cell = registry.cell(source).unwrap();
    let key = cell.key().unwrap();
    cell.write().record(
        [7; 16],
        value([1; 16], 20, true),
        Some(SessionSequenceValue {
            object_id: [1; 16],
            value: 23,
        }),
        true,
    );
    let first_control = StorageReadControl::with_limit(4096);
    let first = registry.capture(&first_control).unwrap();
    assert!(key.retained(&first).unwrap().is_none());
    cell.write().record(
        [7; 16],
        value([1; 16], -9, false),
        Some(SessionSequenceValue {
            object_id: [1; 16],
            value: 23,
        }),
        false,
    );
    let first_saved = key.captured(&first).unwrap();
    assert_eq!(first_saved.get().unwrap()[0].state.current, 20);
    let second_control = StorageReadControl::with_limit(4096);
    let second = registry.capture(&second_control).unwrap();
    let cancelled = StorageReadControl::with_limit(4096);
    let failed = registry.capture(&cancelled).unwrap();
    cancelled.cancellation().cancel();
    cell.write().discard_session_values();
    {
        let source = cell.read();
        let history = &source.values()[&[1; 16]];
        assert!(history.session_currval.is_none());
        assert!(!history.defines_lastval);
        assert_eq!(history.values_by_definition[&[7; 16]].current, -9);
    }
    cell.write()
        .record([8; 16], value([1; 16], 900, true), None, true);
    drop(cell);
    assert!(weak.upgrade().is_none());
    assert!(failed.check().is_err());
    let second_saved = key.captured(&second).unwrap();
    assert_eq!(second_saved.get().unwrap()[0].state.current, -9);
    assert!(!second_saved.get().unwrap()[0].state.called);
    drop((first, second, failed, registry, key));
    assert_eq!(cancelled.memory().used(), 0);
    first_control.cancellation().cancel();
    assert!(first_saved.get().is_err());
    assert_eq!(second_saved.get().unwrap()[0].state.current, -9);
    drop((first_saved, second_saved));
    assert_eq!(first_control.memory().used(), 0);
    assert_eq!(second_control.memory().used(), 0);
}
