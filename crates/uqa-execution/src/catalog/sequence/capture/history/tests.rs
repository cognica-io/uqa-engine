//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::sequence::registry::{SequenceCatalog, SequenceRecord};
use crate::catalog::{security::roles::persistence::RoleCatalogSnapshot, sequence::SequenceState};
use std::sync::Arc;
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
        catalog: Arc::new(
            SequenceCatalog::try_from_iter([(
                relation,
                SequenceRecord {
                    state,
                    object_id: [1; 16],
                    persistence: RelationPersistence::Permanent,
                    catalog_oid: Some(32_001),
                },
                BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
            )])
            .unwrap(),
        ),
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

fn captured_state(value: &CapturedMemoryValue<CapturedSequences>) -> SequenceState {
    value.get().unwrap().iter().next().unwrap().state
}

#[test]
fn sequence_value_capture_joins_original_object_and_definition_before_admission() {
    let mut source = fixture();
    source
        .record([7; 16], value([1; 16], i64::MIN, false), None, false)
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let mut saved = None;
    let original_allocations =
        allocation_counter::measure(|| saved = Some(source.freeze(&control).unwrap()));
    let saved = saved.unwrap();
    let mut expected = source
        .definitions
        .as_ref()
        .unwrap()
        .catalog
        .values()
        .next()
        .copied()
        .unwrap();
    expected.current = i64::MIN;
    expected.called = false;
    expected.log_count = 19;
    assert_eq!(captured_state(&saved), expected);
    assert_eq!(
        saved.get().unwrap().iter().next().unwrap().object_id,
        [1; 16]
    );
    assert_eq!(
        saved.get().unwrap().iter().next().unwrap().catalog_oid,
        Some(32_001)
    );
    // A replaced definition and an unrelated object cannot update the selected original generation.
    source
        .record([8; 16], value([1; 16], 999, true), None, true)
        .unwrap();
    source
        .record([7; 16], value([2; 16], 777, true), None, true)
        .unwrap();
    let temporary = RelationIdentity::new("pg_temp_1", "x".repeat(1 << 20));
    let definitions = source.definitions.as_mut().unwrap();
    Arc::make_mut(&mut definitions.catalog)
        .insert(
            temporary,
            SequenceRecord {
                state: expected,
                object_id: [4; 16],
                persistence: RelationPersistence::Temporary,
                catalog_oid: None,
            },
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    for object in 3_u128..1_003 {
        source
            .record([7; 16], value(object.to_le_bytes(), 100, true), None, false)
            .unwrap();
    }
    let second = StorageReadControl::with_limit(4096);
    let mut other = None;
    let with_excluded =
        allocation_counter::measure(|| other = Some(source.freeze(&second).unwrap()));
    assert_eq!(original_allocations.count_total, 0);
    assert_eq!(with_excluded.count_total, 0);
    assert_eq!(original_allocations.bytes_total, with_excluded.bytes_total);
    assert_eq!(original_allocations.count_total, with_excluded.count_total);
    assert_eq!(control.memory().used(), second.memory().used());
    assert_eq!(
        other
            .as_ref()
            .unwrap()
            .get()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .state,
        expected
    );
    let required = control.memory().used();
    drop((saved, other));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(second.memory().used(), 0);
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(source.freeze(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let exact = StorageReadControl::with_limit(required);
    let retained = source.freeze(&exact).unwrap();
    assert_eq!(captured_state(&retained), expected);
    drop(source);
    assert_eq!(captured_state(&retained), expected);
    drop(retained);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn sequence_history_publication_preserves_unread_cuts_and_native_session_observations() {
    let registry = MemoryCaptureRegistry::new();
    let source = fixture();
    let weak = Arc::downgrade(&source.definitions.as_ref().unwrap().catalog);
    let cell = registry.cell(source).unwrap();
    let key = cell.key().unwrap();
    cell.write()
        .record(
            [7; 16],
            value([1; 16], 20, true),
            Some(SessionSequenceValue {
                object_id: [1; 16],
                value: 23,
            }),
            true,
        )
        .unwrap();
    let first_control = StorageReadControl::with_limit(4096);
    let first = registry.capture(&first_control).unwrap();
    assert!(key.retained(&first).unwrap().is_none());
    cell.write()
        .record(
            [7; 16],
            value([1; 16], -9, false),
            Some(SessionSequenceValue {
                object_id: [1; 16],
                value: 23,
            }),
            false,
        )
        .unwrap();
    let first_saved = key.captured(&first).unwrap();
    assert_eq!(captured_state(&first_saved).current, 20);
    let second_control = StorageReadControl::with_limit(4096);
    let second = registry.capture(&second_control).unwrap();
    let cancelled = StorageReadControl::with_limit(4096);
    let failed = registry.capture(&cancelled).unwrap();
    cancelled.cancellation().cancel();
    cell.write().discard_session_values();
    {
        let source = cell.read();
        let history = source.values();
        assert_eq!(history.observations().count(), 0);
        assert_eq!(history.lastval(), None);
        assert_eq!(history.position([1; 16], [7; 16]).unwrap().current, -9);
    }
    cell.write()
        .record([8; 16], value([1; 16], 900, true), None, true)
        .unwrap();
    drop(cell);
    assert!(weak.upgrade().is_none());
    assert!(failed.check().is_err());
    let second_saved = key.captured(&second).unwrap();
    assert_eq!(captured_state(&second_saved).current, -9);
    assert!(!captured_state(&second_saved).called);
    drop((first, second, failed, registry, key));
    assert_eq!(cancelled.memory().used(), 0);
    first_control.cancellation().cancel();
    assert!(first_saved.get().is_err());
    assert_eq!(captured_state(&second_saved).current, -9);
    drop((first_saved, second_saved));
    assert_eq!(first_control.memory().used(), 0);
    assert_eq!(second_control.memory().used(), 0);
}

#[test]
fn sequence_history_prepares_every_successor_before_effects_and_preserves_abandoned_roots() {
    let memory = uqa_core::memory::MemoryBudget::new(1 << 20);
    let original = fixture();
    let mut catalog = SequenceCatalog::with_budget(&memory);
    for (relation, record) in original.definitions.as_ref().unwrap().catalog.iter() {
        catalog
            .insert(
                relation.clone(),
                *record,
                original.definitions.as_ref().unwrap().catalog.security()[relation].clone(),
            )
            .unwrap();
    }
    let mut definitions = original.definitions.unwrap();
    definitions.catalog = Arc::new(catalog);
    let mut source = SequenceCaptureSource::new(definitions);
    source
        .record(
            [7; 16],
            value([1; 16], 17, true),
            Some(SessionSequenceValue {
                object_id: [1; 16],
                value: 11,
            }),
            true,
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let retained = source.freeze(&control).unwrap();
    let used = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let mut effects = 0;
    let result = (|| -> Result<(), uqa_sql::SQLError> {
        let prepared = source.prepare([1; 16], [7; 16])?;
        effects += 1;
        prepared.publish(value([1; 16], 43, true), None, false);
        Ok(())
    })();
    assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(effects, 0);
    assert_eq!(
        source.values().position([1; 16], [7; 16]).unwrap().current,
        17
    );
    assert_eq!(
        source
            .values()
            .observations()
            .next()
            .unwrap()
            .1
            .unwrap()
            .value,
        11
    );
    drop(occupied);
    assert_eq!(memory.used(), used);
    drop(source.prepare([1; 16], [7; 16]).unwrap());
    assert_eq!(memory.used(), used);
    let prepared = source.prepare([1; 16], [7; 16]).unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    effects += 1;
    prepared.publish(
        value([1; 16], 43, true),
        Some(SessionSequenceValue {
            object_id: [1; 16],
            value: 31,
        }),
        false,
    );
    drop(occupied);
    assert_eq!(effects, 1);
    assert_eq!(
        source.values().position([1; 16], [7; 16]).unwrap().current,
        43
    );
    assert_eq!(source.values().lastval(), Some([1; 16]));
    assert_eq!(
        source
            .values()
            .observations()
            .next()
            .unwrap()
            .1
            .unwrap()
            .value,
        31
    );
    assert_eq!(captured_state(&retained).current, 17);
    drop((source, retained));
    assert_eq!(memory.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
