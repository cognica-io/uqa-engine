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

fn sample_sequence_acl() -> Vec<crate::catalog::SequenceAclEntry> {
    vec![crate::catalog::SequenceAclEntry {
        role: "sequence_reader".into(),
        grantor: Some("sequence_owner".into()),
        privileges: crate::catalog::SequencePrivileges {
            select: true,
            update: false,
            usage: true,
        },
        grant_options: crate::catalog::SequencePrivileges {
            usage: true,
            ..crate::catalog::SequencePrivileges::default()
        },
    }]
}

#[test]
fn schema_rows_decode_legacy_names_and_round_trip_security_metadata() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    store
        .put(
            &single_str_key(TAG_SCHEMA, "archive").unwrap(),
            &string_value("archive"),
        )
        .unwrap();
    assert_eq!(
        catalog.load_schema_rows().unwrap(),
        vec![SchemaRow::legacy("archive")]
    );

    let schema = SchemaRow {
        name: "archive".into(),
        role_owner: "archive_owner".into(),
        acl: Some(vec![crate::catalog::SchemaAclEntry {
            role: "archive_writer".into(),
            grantor: Some("archive_owner".into()),
            privileges: crate::catalog::SchemaPrivileges::ALL,
            grant_options: crate::catalog::SchemaPrivileges {
                usage: false,
                create: true,
            },
        }]),
    };
    catalog.save_schema_row(&schema).unwrap();
    assert_eq!(catalog.load_schema_rows().unwrap(), vec![schema]);
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
    let owner = crate::catalog::SequenceOwner {
        table_object_id: [5; 16],
        column_object_id: [6; 16],
        dependency: crate::catalog::SequenceOwnerDependency::Internal,
    };
    let acl = sample_sequence_acl();
    catalog.save_schema("public").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "controlled"),
            role_owner: "sequence_owner".into(),
            acl: Some(acl.clone()),
            object_id,
            definition_generation: object_id,
            start: 1,
            increment: 2,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: Some(owner),
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
            .set_sequence_value("public.controlled", object_id, 7, false, 0)
            .unwrap(),
        Some(7)
    );
    let uncalled = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(uncalled.current, 7);
    assert!(!uncalled.called);
    assert_eq!(uncalled.owner, Some(owner));
    assert_eq!(uncalled.role_owner, "sequence_owner");
    assert_eq!(uncalled.acl, Some(acl));
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
            .set_sequence_value("public.controlled", object_id, 20, true, 0)
            .unwrap(),
        Some(20)
    );
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(22)
    );
}

#[test]
fn sequence_reservations_cycle_at_the_configured_bounds() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store);
    let cycling_id = [9; 16];
    catalog.save_schema("public").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "cycling"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: cycling_id,
            definition_generation: cycling_id,
            start: 5,
            increment: 3,
            current: 5,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: crate::catalog::SequenceOptions {
                data_type: "integer".into(),
                min_value: Some(2),
                max_value: Some(5),
                cycle: true,
                cache_size: 1,
            },
            owner: None,
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
fn sequence_rename_moves_catalog_identity_and_value_atomically() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(store);
    let object_id = [17; 16];
    catalog.save_schema("public").unwrap();
    catalog.save_schema("archive").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "ids"),
            role_owner: "uqa".into(),
            acl: None,
            object_id,
            definition_generation: [18; 16],
            start: 1,
            increment: 1,
            current: 7,
            called: true,
            log_count: 0,
            persistence: "u".into(),
            options: SequenceOptions {
                cache_size: 3,
                ..SequenceOptions::default()
            },
            owner: None,
        })
        .unwrap();

    assert!(catalog
        .rename_sequence_row("public.ids", "archive.renamed_ids")
        .unwrap());
    assert_eq!(
        catalog
            .next_sequence_value("public.ids", object_id)
            .unwrap(),
        None
    );
    assert_eq!(
        catalog
            .next_sequence_value("archive.renamed_ids", object_id)
            .unwrap(),
        Some(8)
    );
    let row = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(
        row.relation,
        RelationIdentity::new("archive", "renamed_ids")
    );
    assert_eq!(row.object_id, object_id);
    assert_eq!(row.definition_generation, [18; 16]);
    assert_eq!(row.persistence, "u");

    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "occupied"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [19; 16],
            definition_generation: [19; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    assert!(catalog
        .rename_sequence_row("archive.renamed_ids", "public.occupied")
        .unwrap_err()
        .to_string()
        .contains("already exists"));
    assert_eq!(catalog.load_sequence_rows().unwrap().len(), 2);
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
            role_owner: "uqa".into(),
            acl: None,
            object_id,
            definition_generation: generation,
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions {
                min_value: Some(1),
                max_value: Some(3),
                cache_size: 5,
                ..SequenceOptions::default()
            },
            owner: None,
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
            log_count: 0,
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
            log_count: 0,
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
fn relation_namespace_migration_gives_catalog_indexes_typed_parents() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    store
        .put(
            &single_str_key(TAG_TABLE, "app.docs").unwrap(),
            &legacy_table_value("app.docs"),
        )
        .unwrap();
    store
        .put(
            &single_str_key(TAG_CATALOG_INDEX, "shared_idx").unwrap(),
            &encode_value(&StoredCatalogIndex {
                index_type: "btree".into(),
                table_name: "app.docs".into(),
                columns_json: "[\"id\"]".into(),
                parameters_json: "{}".into(),
            })
            .unwrap(),
        )
        .unwrap();
    store
        .put(
            &single_str_key(TAG_CATALOG_INDEX, "shared.dot").unwrap(),
            &encode_value(&StoredCatalogIndex {
                index_type: "btree".into(),
                table_name: "app.docs".into(),
                columns_json: "[\"id\"]".into(),
                parameters_json: "{}".into(),
            })
            .unwrap(),
        )
        .unwrap();
    store
        .put(
            &single_str_key(TAG_CATALOG_INDEX, "temp_idx").unwrap(),
            &encode_value(&StoredCatalogIndex {
                index_type: "btree".into(),
                table_name: "pg_temp_91.docs".into(),
                columns_json: "[\"id\"]".into(),
                parameters_json: "{}".into(),
            })
            .unwrap(),
        )
        .unwrap();

    catalog.migrate_relation_namespace().unwrap();
    catalog.migrate_relation_namespace().unwrap();

    let rows = catalog.load_catalog_indexes().unwrap();
    assert_eq!(rows.len(), 2);
    for relation in [
        RelationIdentity::new("app", "shared_idx"),
        RelationIdentity::new("app", "shared.dot"),
    ] {
        assert!(rows
            .iter()
            .any(|row| row.relation == relation && row.table_name == "app.docs"));
        let parent = store
            .get(&relation_key(TAG_RELATION, &relation).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            decode_value::<StoredRelation>(&parent).unwrap().kind,
            RelationKind::Index
        );
    }
    assert!(store
        .get(&single_str_key(TAG_CATALOG_INDEX, "shared_idx").unwrap())
        .unwrap()
        .is_none());
    assert!(store
        .get(&single_str_key(TAG_CATALOG_INDEX, "shared.dot").unwrap())
        .unwrap()
        .is_none());
    assert!(store
        .get(&single_str_key(TAG_CATALOG_INDEX, "temp_idx").unwrap())
        .unwrap()
        .is_none());

    store
        .delete(&relation_key(TAG_RELATION, &RelationIdentity::new("app", "shared_idx")).unwrap())
        .unwrap();
    let error = catalog.migrate_relation_namespace().unwrap_err();
    assert!(error.to_string().contains("has no index relation parent"));
    assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 2);
}

#[test]
fn relation_namespace_migration_rejects_catalog_index_collisions_atomically() {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    for name in ["app.docs", "app.taken"] {
        store
            .put(
                &single_str_key(TAG_TABLE, name).unwrap(),
                &legacy_table_value(name),
            )
            .unwrap();
    }
    let legacy_index_key = single_str_key(TAG_CATALOG_INDEX, "taken").unwrap();
    store
        .put(
            &legacy_index_key,
            &encode_value(&StoredCatalogIndex {
                index_type: "btree".into(),
                table_name: "app.docs".into(),
                columns_json: "[\"id\"]".into(),
                parameters_json: "{}".into(),
            })
            .unwrap(),
        )
        .unwrap();

    let error = catalog.migrate_relation_namespace().unwrap_err();
    assert!(error.to_string().contains("migration collision"));
    assert!(error.to_string().contains("app.taken"));
    assert!(store
        .scan_prefix(&key_with_tag(TAG_RELATION))
        .unwrap()
        .is_empty());
    assert!(store.get(&legacy_index_key).unwrap().is_some());
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
                        role_owner: "uqa".into(),
                        acl: None,
                        object_id: [0; 16],
                        definition_generation: [0; 16],
                        start: 1,
                        increment: 1,
                        current: 0,
                        called: true,
                        log_count: 0,
                        persistence: "p".into(),
                        options: SequenceOptions::default(),
                        owner: None,
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
