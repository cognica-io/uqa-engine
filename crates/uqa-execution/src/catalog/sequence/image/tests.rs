//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    security::roles::persistence::RoleCatalogSnapshot,
    sequence::{
        capture::{durable_sequences, SequenceCaptureSource},
        registry::{SequenceCatalog, SequenceRecord},
        session::NontransactionalSequenceValue,
        snapshot::SequenceReadSnapshot,
        SequenceState,
    },
};
use std::sync::Arc;
use uqa_core::{
    catalog_role::RoleIdentity,
    catalog_sequence::{SequenceOwner, SequenceOwnerDependency},
    memory::{MemoryBudget, ProductionControl},
    RelationIdentity,
};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::security::BoundSequenceSecurity,
};
use uqa_storage::{memory_capture::MemoryCaptureSource, read_control::StorageReadControl};

mod validation;

#[test]
fn sequence_image_native_clones_and_captures_retain_decoded_allowance() {
    let (metadata, authority) = encode(&fixture());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(65536);
    let restored = SequenceCatalog::read_images(
        &mut metadata.as_slice(),
        &mut authority.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    let used = memory.used();
    let clone = restored.clone();
    drop(restored);
    assert_eq!(memory.used(), used);
    let source = SequenceReadSnapshot {
        catalog: Arc::new(clone),
        roles: RoleCatalogSnapshot {
            roles: Arc::default(),
            memberships: Arc::default(),
        },
    };
    let control = StorageReadControl::with_limit(65536);
    let captured = source.freeze(&control).unwrap();
    drop(source);
    assert!(memory.used() > 0);
    assert!(control.memory().used() > 0);
    assert_eq!(
        encode_captured(captured.get().unwrap(), &token),
        (metadata, authority)
    );
    drop(captured);
    assert_eq!(memory.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

fn fixture() -> SequenceReadSnapshot {
    let state = SequenceState {
        start: 1,
        increment: 3,
        current: 41,
        called: true,
        log_count: 19,
        data_type: SequenceDataType::BigInt,
        min_value: i64::MIN,
        max_value: i64::MAX,
        cycle: true,
        cache_size: 13,
        definition_generation: [7; 16],
        owner: Some(SequenceOwner {
            table_object_id: [8; 16],
            column_object_id: [9; 16],
            dependency: SequenceOwnerDependency::Internal,
        }),
    };
    SequenceReadSnapshot {
        catalog: Arc::new(
            SequenceCatalog::try_from_iter([(
                RelationIdentity::new("s", "q"),
                SequenceRecord {
                    state,
                    object_id: [2; 16],
                    catalog_oid: Some(31_001),
                    persistence: RelationPersistence::Permanent,
                },
                BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
            )])
            .unwrap(),
        ),
        roles: RoleCatalogSnapshot {
            roles: Arc::default(),
            memberships: Arc::default(),
        },
    }
}

fn encode(source: &SequenceReadSnapshot) -> (Vec<u8>, Vec<u8>) {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let captured =
        durable_sequences(source, &ProductionControl::new(&memory, &token, &token)).unwrap();
    encode_captured(&captured, &token)
}

fn encode_captured(captured: &CapturedSequences, token: &CancellationToken) -> (Vec<u8>, Vec<u8>) {
    let mut metadata = vec![];
    let mut authority = vec![];
    captured
        .image_reader(token)
        .unwrap()
        .write_to(&mut metadata)
        .unwrap();
    captured
        .authority_image_reader(token)
        .unwrap()
        .write_to(&mut authority)
        .unwrap();
    (metadata, authority)
}

fn assert_same(source: &SequenceReadSnapshot, restored: &SequenceCatalog) {
    assert!(source.catalog.iter().eq(restored.iter()));
    assert_eq!(source.catalog.security(), restored.security());
    assert!(source
        .catalog
        .catalog_oids()
        .iter()
        .eq(restored.catalog_oids().iter()));
}

#[test]
fn sequence_image_matches_independent_wire_and_native_metadata() {
    let source = fixture();
    let (metadata, authority) = encode(&source);
    let mut expected = b"UQASEQ01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0s\x01\0\0\0\0\0\0\0q".to_vec();
    expected.extend_from_slice(&[2; 16]);
    expected.extend_from_slice(b"\x01\x19\x79\0\0\0\x02\x01\x01\x02");
    expected.extend_from_slice(&[8; 16]);
    expected.extend_from_slice(&[9; 16]);
    expected.extend_from_slice(&[7; 16]);
    expected.extend_from_slice(&[
        1, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 41, 0, 0, 0, 0, 0, 0, 0, 19, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 255, 255, 255, 255, 255, 255, 255, 127, 13, 0, 0, 0, 0, 0,
        0, 0,
    ]);
    assert_eq!(metadata, expected);
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let restored = SequenceCatalog::read_images(
        &mut metadata.as_slice(),
        &mut authority.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    assert_same(&source, &restored);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_images_keep_all_numeric_boundaries_types_persistence_and_owner_modes() {
    for data_type in [
        SequenceDataType::SmallInt,
        SequenceDataType::Integer,
        SequenceDataType::BigInt,
    ] {
        for persistence in [
            RelationPersistence::Permanent,
            RelationPersistence::Unlogged,
        ] {
            for owner in [
                None,
                Some(SequenceOwnerDependency::Automatic),
                Some(SequenceOwnerDependency::Internal),
            ] {
                let mut source = fixture();
                let relation = RelationIdentity::new("s", "q");
                let catalog = Arc::make_mut(&mut source.catalog);
                let mut record = *catalog.get(&relation).unwrap();
                let (min, max) = data_type.bounds();
                record.state = SequenceState {
                    start: min,
                    increment: -1,
                    current: max,
                    called: false,
                    min_value: min,
                    max_value: max,
                    data_type,
                    owner: owner.map(|dependency| SequenceOwner {
                        dependency,
                        table_object_id: [3; 16],
                        column_object_id: [4; 16],
                    }),
                    ..record.state
                };
                record.persistence = persistence;
                record.catalog_oid = None;
                catalog.remove(&relation).unwrap();
                catalog
                    .insert(
                        relation,
                        record,
                        BoundSequenceSecurity {
                            role_owner: RoleIdentity::BOOTSTRAP,
                            acl: Some(vec![]),
                        },
                    )
                    .unwrap();
                let (metadata, authority) = encode(&source);
                let memory = MemoryBudget::new(65536);
                let token = CancellationToken::new();
                let restored = SequenceCatalog::read_images(
                    &mut metadata.as_slice(),
                    &mut authority.as_slice(),
                    &memory,
                    &token,
                )
                .unwrap();
                assert_same(&source, &restored);
                drop(restored);
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn sequence_image_retains_selected_autonomous_position_after_source_retirement() {
    let mut source = SequenceCaptureSource::new(fixture());
    source
        .record(
            [7; 16],
            NontransactionalSequenceValue {
                object_id: [2; 16],
                current: i64::MAX,
                called: true,
                log_count: 31,
                autonomous: true,
            },
            None,
            false,
        )
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    let captured = source.freeze(&control).unwrap();
    source
        .record(
            [7; 16],
            NontransactionalSequenceValue {
                object_id: [2; 16],
                current: 99,
                called: false,
                log_count: 0,
                autonomous: true,
            },
            None,
            false,
        )
        .unwrap();
    drop(source);
    let token = CancellationToken::new();
    let (metadata, authority) = encode_captured(captured.get().unwrap(), &token);
    let memory = MemoryBudget::new(65536);
    let restored = SequenceCatalog::read_images(
        &mut metadata.as_slice(),
        &mut authority.as_slice(),
        &memory,
        &token,
    )
    .unwrap();
    let state = restored.values().next().unwrap();
    assert_eq!(
        (state.current, state.called, state.log_count),
        (i64::MAX, true, 31)
    );
    drop((captured, restored));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_image_preparation_and_fragmented_output_allocate_no_payloads() {
    let mut source = fixture();
    let from = RelationIdentity::new("s", "q");
    Arc::make_mut(&mut source.catalog)
        .rename(
            &from,
            &RelationIdentity::new("Quoted.Schema", "x".repeat(65536)),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = source.freeze(&control).unwrap();
    let token = CancellationToken::new();
    let expected = encode(&source).0;
    drop(source);
    for width in [1, 7, 4096, 20000] {
        let mut actual = Vec::with_capacity(expected.len());
        let mut buffer = vec![0; width];
        let allocations = allocation_counter::measure(|| {
            let mut reader = captured.get().unwrap().image_reader(&token).unwrap();
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            loop {
                let count = reader.read(&mut buffer).unwrap();
                assert!(count <= 16384);
                if count == 0 {
                    break;
                }
                actual.extend_from_slice(&buffer[..count]);
            }
            assert_eq!(reader.read(&mut buffer).unwrap(), 0);
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(actual, expected);
    }
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}
