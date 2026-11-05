//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_role::RoleIdentity, CancellationToken};
use uqa_sql::ast::{RelationPersistence, SequenceDataType};

fn record(index: usize) -> SequenceRecord {
    let mut state = super::super::SequenceState::initial(1, 3, SequenceDataType::BigInt);
    state.definition_generation = [7; 16];
    state.cache_size = 13;
    state.log_count = 31;
    SequenceRecord {
        state,
        object_id: u128::try_from(index).unwrap().to_le_bytes(),
        persistence: RelationPersistence::Permanent,
        catalog_oid: Some(32_000 + u32::try_from(index).unwrap()),
    }
}

fn relation(index: usize) -> RelationIdentity {
    RelationIdentity::new("Quoted.Schema", format!("Sequence.{index:05}"))
}

fn security() -> BoundSequenceSecurity {
    BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP)
}

#[test]
fn native_sequence_capture_keeps_complete_pairs_without_cardinality_work() {
    for count in [1, 64, 4096] {
        let native = MemoryBudget::new(64 << 20);
        let mut source = SequenceCatalog::with_budget(&native);
        for index in 0..count {
            source
                .insert(relation(index), record(index), security())
                .unwrap();
        }
        source.catalog_oids.insert([255; 16], 99_999, 0).unwrap();
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(source.capture(&control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        let saved = saved.unwrap();
        let required = memory.used();
        let mut temporary = record(count);
        temporary.persistence = RelationPersistence::Temporary;
        source
            .insert(
                RelationIdentity::new("public", "t".repeat(1 << 20)),
                temporary,
                security(),
            )
            .unwrap();
        let exact = MemoryBudget::new(required);
        let independent = source
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), required);
        assert_eq!(saved.records.len(), count);
        assert_eq!(saved.security.len(), count);
        let denied = MemoryBudget::new(required - 1);
        assert!(source
            .capture(&ProductionControl::new(&denied, &token, &token))
            .is_err());
        assert_eq!(denied.used(), 0);
        for (key, row) in saved.records.iter() {
            assert!(std::ptr::eq(row, source.get(key).unwrap()));
            assert!(std::ptr::eq(row, independent.records.get(key).unwrap()));
            assert_eq!(
                saved.security.get(key).unwrap().security(),
                source.security().get(key).unwrap()
            );
        }
        source.remove(&relation(0)).unwrap();
        assert_eq!(saved.records.get(&relation(0)), Some(&record(0)));
        assert_eq!(
            saved.security.get(&relation(0)).unwrap().security(),
            &security()
        );
        drop((source, saved));
        assert_eq!(memory.used(), 0);
        assert!(native.used() > 0);
        drop(independent);
        assert_eq!(exact.used(), 0);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn native_sequence_position_precedes_external_effects_and_needs_no_late_admission() {
    let native = MemoryBudget::new(1 << 20);
    let mut source = SequenceCatalog::with_budget(&native);
    source.insert(relation(0), record(0), security()).unwrap();
    let original = source.clone();
    let used = native.used();
    let occupied = native.reserve(native.available()).unwrap();
    let mut effects = 0;
    let result = (|| -> Result<(), SQLError> {
        let prepared = source
            .prepare_position(&relation(0), record(0).object_id, [7; 16])?
            .unwrap();
        effects += 1;
        prepared.publish(uqa_storage::SequenceValuePosition {
            current: 43,
            called: true,
            log_count: 19,
        });
        Ok(())
    })();
    assert!(result.is_err());
    assert_eq!(effects, 0);
    assert!(std::ptr::eq(
        source.get(&relation(0)).unwrap(),
        original.get(&relation(0)).unwrap()
    ));
    drop(occupied);
    assert_eq!(native.used(), used);
    assert!(source
        .prepare_position(&relation(0), record(0).object_id, [8; 16])
        .unwrap()
        .is_none());
    drop(
        source
            .prepare_position(&relation(0), record(0).object_id, [7; 16])
            .unwrap(),
    );
    assert_eq!(native.used(), used);
    let prepared = source
        .prepare_position(&relation(0), record(0).object_id, [7; 16])
        .unwrap()
        .unwrap();
    assert_eq!(prepared.state(), record(0).state);
    let occupied = native.reserve(native.available()).unwrap();
    effects += 1;
    prepared.publish(uqa_storage::SequenceValuePosition {
        current: 43,
        called: true,
        log_count: 19,
    });
    drop(occupied);
    assert_eq!(effects, 1);
    let row = source.get(&relation(0)).unwrap();
    assert_eq!(row.state.current, 43);
    assert!(row.state.called);
    assert_eq!(row.state.log_count, 19);
    assert_eq!(row.state.definition_generation, [7; 16]);
    assert_eq!(row.catalog_oid, record(0).catalog_oid);
    assert_eq!(original.get(&relation(0)).unwrap(), &record(0));
    assert_eq!(
        source.security().get(&relation(0)),
        original.security().get(&relation(0))
    );
    drop((source, original));
    assert_eq!(native.used(), 0);
}

#[test]
fn native_sequence_generation_identity_changes_only_with_published_roots() {
    let mut source = SequenceCatalog::default();
    source.insert(relation(0), record(0), security()).unwrap();
    let original = source.clone();
    assert!(source.shares_generation(&original));
    drop(
        source
            .prepare_position(&relation(0), record(0).object_id, [7; 16])
            .unwrap(),
    );
    assert!(source.shares_generation(&original));
    for change in ["value", "authority", "oid", "name", "membership"] {
        let mut next = source.clone();
        match change {
            "value" => next
                .prepare_position(&relation(0), record(0).object_id, [7; 16])
                .unwrap()
                .unwrap()
                .publish(uqa_storage::SequenceValuePosition {
                    current: 99,
                    called: true,
                    log_count: 0,
                }),
            "authority" => {
                let mut authority = security();
                authority.acl = Some(Vec::new());
                next.replace_security(&relation(0), authority).unwrap();
            }
            "oid" => next
                .replace_catalog_oids([(record(0).object_id, 41_000)])
                .unwrap(),
            "name" => next.rename(&relation(0), &relation(1)).unwrap(),
            _ => {
                next.remove(&relation(0)).unwrap();
            }
        }
        assert!(!next.shares_generation(&original), "{change}");
        assert_eq!(source.get(&relation(0)), Some(&record(0)));
        assert!(source.shares_generation(&original));
    }
}
