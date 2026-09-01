//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::records::LEGACY_VIEWS_METADATA_KEY;
use super::*;
use crate::document_store::Document;
use crate::key_value::index_keys::{
    btree_entry_key, btree_index_key, hnsw_metadata_key, hnsw_node_key, ivf_assignment_key,
    ivf_centroid_key, ivf_metadata_key,
};
use crate::key_value::MemoryKeyValueStore;

fn legacy_table_value(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "analyzer_json": "{}",
        "fts_fields": [],
        "vector_fields": [],
        "columns_json": "[]",
        "constraints_json": ""
    }))
    .unwrap()
}

#[test]
fn relation_namespace_migration_is_one_batch_and_moves_public_data() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    store
        .put(
            &single_str_key(TAG_TABLE, "docs").unwrap(),
            &legacy_table_value("docs"),
        )
        .unwrap();
    store
        .put(
            &single_str_key(TAG_SEQUENCE, "seq").unwrap(),
            br#"{"start":1,"increment":1,"current":0}"#,
        )
        .unwrap();
    store
        .put(
            &document_key_prefix("docs").unwrap(),
            &encode_value(&Document::new()).unwrap(),
        )
        .unwrap();
    catalog
        .set_metadata(LEGACY_VIEWS_METADATA_KEY, r#"{"report":{"plan":1}}"#)
        .unwrap();

    catalog.migrate_relation_namespace().unwrap();

    assert_eq!(
        catalog.load_tables().unwrap()[0].relation,
        RelationIdentity::new("public", "docs")
    );
    let sequence = &catalog.load_sequence_rows().unwrap()[0];
    assert_eq!(sequence.relation, RelationIdentity::new("public", "seq"));
    assert_eq!(sequence.object_id, [0; 16]);
    assert_eq!(
        catalog
            .next_sequence_value("public.seq", sequence.object_id)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        catalog.load_views().unwrap()[0].relation,
        RelationIdentity::new("public", "report")
    );
    assert!(catalog
        .load_schemas()
        .unwrap()
        .contains(&"public".to_string()));
    assert!(store
        .get(&document_key_prefix("public.docs").unwrap())
        .unwrap()
        .is_some());
    assert!(store
        .get(&document_key_prefix("docs").unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn sequence_set_value_preserves_the_next_allocation_state() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store);
    let object_id = [7; 16];
    catalog.save_schema("public").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "controlled"),
            object_id,
            definition_generation: object_id,
            start: 1,
            increment: 2,
            current: 1,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions::default(),
        })
        .unwrap();

    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", [8; 16])
            .unwrap(),
        None
    );
    assert_eq!(
        catalog
            .set_sequence_value("public.controlled", object_id, 7, false)
            .unwrap(),
        Some(7)
    );
    let uncalled = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(uncalled.current, 7);
    assert!(!uncalled.called);
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(7)
    );
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(9)
    );
    assert_eq!(
        catalog
            .set_sequence_value("public.controlled", object_id, 20, true)
            .unwrap(),
        Some(20)
    );
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(22)
    );

    let cycling_id = [9; 16];
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "cycling"),
            object_id: cycling_id,
            definition_generation: cycling_id,
            start: 5,
            increment: 3,
            current: 5,
            called: false,
            persistence: "p".into(),
            options: crate::catalog::SequenceOptions {
                data_type: "integer".into(),
                min_value: Some(2),
                max_value: Some(5),
                cycle: true,
                cache_size: 1,
            },
        })
        .unwrap();
    for expected in [5, 2, 5, 2] {
        assert_eq!(
            catalog
                .next_sequence_value("public.cycling", cycling_id)
                .unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn sequence_reservations_are_atomic_and_stop_at_the_configured_bound() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store);
    let object_id = [11; 16];
    let generation = [12; 16];
    catalog.save_schema("public").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "cached"),
            object_id,
            definition_generation: generation,
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions {
                min_value: Some(1),
                max_value: Some(3),
                cache_size: 5,
                ..SequenceOptions::default()
            },
        })
        .unwrap();

    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::Reserved(crate::catalog::SequenceValueReservation {
            first_value: 1,
            last_value: 3,
            count: 3,
        })
    );
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::Exhausted
    );
    let mut row = catalog.load_sequence_rows().unwrap().remove(0);
    row.options.cycle = true;
    row.definition_generation = [13; 16];
    assert!(catalog.replace_sequence_row(&row).unwrap());
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::DefinitionChanged
    );
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, [13; 16])
            .unwrap(),
        SequenceReservationResult::Reserved(crate::catalog::SequenceValueReservation {
            first_value: 1,
            last_value: 3,
            count: 3,
        })
    );
}

#[test]
fn relation_namespace_migration_moves_all_physical_index_namespaces() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    store
        .put(
            &single_str_key(TAG_TABLE, "docs").unwrap(),
            &legacy_table_value("docs"),
        )
        .unwrap();
    let physical_keys = [
        (
            btree_index_key("docs", "rank").unwrap(),
            btree_index_key("public.docs", "rank").unwrap(),
        ),
        (
            btree_entry_key("docs", "rank", 1).unwrap(),
            btree_entry_key("public.docs", "rank", 1).unwrap(),
        ),
        (
            ivf_metadata_key("docs", "ivf_vector").unwrap(),
            ivf_metadata_key("public.docs", "ivf_vector").unwrap(),
        ),
        (
            ivf_centroid_key("docs", "ivf_vector", 0).unwrap(),
            ivf_centroid_key("public.docs", "ivf_vector", 0).unwrap(),
        ),
        (
            ivf_assignment_key("docs", "ivf_vector", 1, 0).unwrap(),
            ivf_assignment_key("public.docs", "ivf_vector", 1, 0).unwrap(),
        ),
        (
            hnsw_metadata_key("docs", "hnsw_vector").unwrap(),
            hnsw_metadata_key("public.docs", "hnsw_vector").unwrap(),
        ),
        (
            hnsw_node_key("docs", "hnsw_vector", 0).unwrap(),
            hnsw_node_key("public.docs", "hnsw_vector", 0).unwrap(),
        ),
    ];
    for (index, (legacy, _)) in physical_keys.iter().enumerate() {
        store.put(legacy, &[index as u8]).unwrap();
    }

    catalog.migrate_relation_namespace().unwrap();

    for (index, (legacy, canonical)) in physical_keys.iter().enumerate() {
        assert_eq!(store.get(legacy).unwrap(), None);
        assert_eq!(store.get(canonical).unwrap(), Some(vec![index as u8]));
    }
}

#[test]
fn relation_namespace_migration_rejects_alias_and_cross_kind_collisions() {
    for cross_kind in [false, true] {
        let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let catalog = KeyValueCatalog::new(Arc::clone(&store));
        store
            .put(
                &single_str_key(TAG_TABLE, "docs").unwrap(),
                &legacy_table_value("docs"),
            )
            .unwrap();
        if cross_kind {
            store
                .put(
                    &single_str_key(TAG_SEQUENCE, "public.docs").unwrap(),
                    &encode_value(&StoredSequence {
                        object_id: [0; 16],
                        definition_generation: [0; 16],
                        start: 1,
                        increment: 1,
                        current: 0,
                        called: true,
                        persistence: "p".into(),
                        options: SequenceOptions::default(),
                    })
                    .unwrap(),
                )
                .unwrap();
        } else {
            store
                .put(
                    &single_str_key(TAG_TABLE, "public.docs").unwrap(),
                    &legacy_table_value("public.docs"),
                )
                .unwrap();
        }

        let error = catalog.migrate_relation_namespace().unwrap_err();
        assert!(error.to_string().contains("migration collision"));
        assert!(error.to_string().contains("public.docs"));
        assert!(store
            .scan_prefix(&key_with_tag(TAG_RELATION))
            .unwrap()
            .is_empty());
        assert!(store
            .get(&single_str_key(TAG_TABLE, "docs").unwrap())
            .unwrap()
            .is_some());
    }
}
