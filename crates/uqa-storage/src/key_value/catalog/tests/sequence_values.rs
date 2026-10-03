//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The value state of sequence definitions written by earlier releases moves into value records once, and a definition such a release writes afterwards is refused instead of read.

use super::*;

/// A sequence definition as earlier releases wrote it, with its value state.
fn legacy_definition(current: i64, log_count: i64) -> Vec<u8> {
    encode_value(&StoredSequence {
        security: crate::SequenceSecurityRow::bootstrap(),
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: Some(current),
        called: Some(true),
        log_count: Some(log_count),
        persistence: "p".into(),
        owner: None,
        options: SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            ..SequenceOptions::default()
        },
    })
    .unwrap()
}

fn value_state(catalog: &KeyValueCatalog) -> (i64, bool, i64) {
    let row = catalog.load_sequence_rows().unwrap().remove(0);
    (row.current, row.called, row.log_count)
}

#[test]
fn the_value_state_of_a_legacy_definition_moves_into_its_value_record_once() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog.save_schema("public").unwrap();
    let relation = RelationIdentity::new("public", "ids");
    let key = relation_key(TAG_SEQUENCE, &relation).unwrap();
    store.put(&key, &legacy_definition(41, 7)).unwrap();
    // A definition that has not moved yet is read with the value state it holds.
    assert_eq!(value_state(&catalog), (41, true, 7));

    catalog.migrate_sequence_values().unwrap();
    let definition: serde_json::Value =
        serde_json::from_slice(&store.get(&key).unwrap().unwrap()).unwrap();
    for field in ["current", "called", "log_count"] {
        assert!(definition.get(field).is_none(), "{field}");
    }
    assert_eq!(value_state(&catalog), (41, true, 7));
    assert_eq!(
        catalog.next_sequence_value("public.ids", [1; 16]).unwrap(),
        Some(42)
    );
    assert_eq!(value_state(&catalog), (42, true, 6));
    // A second open finds nothing to move.
    catalog.migrate_sequence_values().unwrap();
    assert_eq!(value_state(&catalog), (42, true, 6));

    // An earlier release that rewrites the definition after the move would allocate from stale state; the definition is refused.
    store.put(&key, &legacy_definition(42, 6)).unwrap();
    for error in [
        catalog.load_sequence_rows().unwrap_err(),
        catalog.migrate_sequence_values().unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("written by an earlier release"),
            "{error}"
        );
    }
}

#[test]
fn a_definition_without_value_state_is_refused_by_earlier_releases() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog.save_schema("public").unwrap();
    let mut row = crate::SequenceRow {
        relation: RelationIdentity::new("public", "ids"),
        security: crate::SequenceSecurityRow::bootstrap(),
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: SequenceOptions::default(),
    };
    assert!(catalog.create_sequence_row(&row).unwrap());
    let encoded = store
        .get(&relation_key(TAG_SEQUENCE, &row.relation).unwrap())
        .unwrap()
        .unwrap();
    // Earlier releases require `current`, so they fail to decode the definition instead of allocating from it.
    assert!(!serde_json::from_slice::<serde_json::Value>(&encoded)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("current"));
    row.options.min_value = Some(1);
    row.options.max_value = Some(i64::MAX);
    assert_eq!(catalog.load_sequence_rows().unwrap(), [row]);
}
