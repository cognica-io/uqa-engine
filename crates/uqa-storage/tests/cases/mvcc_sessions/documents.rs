//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic document races and metadata preservation use the storage session's evaluated boundary.

use super::occurrences::InterleavedStore;
use super::*;
use std::sync::atomic::Ordering;
use uqa_core::Value;
use uqa_storage::{
    CatalogFacade, DocumentMetadata, DocumentStore, KeyValueCatalog, KeyValueDocumentStore,
    RelationIdentity, StoredDocument, TableSchema,
};

fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("n".into(), Value::Int(n))])
}
fn schema(name: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: uqa_storage::RelationSecurityRow::legacy("owner"),
        object_id: [0; 16],
        storage_generation: [0; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    }
}

#[test]
fn document_owners_share_the_provider_contract() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    uqa_storage::key_value::conformance::verify_document_ownership(&a, &b).unwrap();
    uqa_storage::key_value::conformance::verify_document_reopen(&b).unwrap();
}

#[test]
fn document_metadata_changes_reject_an_obsolete_put_without_replaying_evaluation() {
    for patch in [false, true] {
        let persistence = Persistence::new();
        let inner = Arc::new(persistence.session(1 << 20));
        let other = Arc::new(persistence.session(1 << 20));
        let observed = Arc::new(InterleavedStore::new(inner.clone()));
        let mut first = KeyValueDocumentStore::new(observed.clone(), "docs");
        let mut second = KeyValueDocumentStore::new(other.clone(), "docs");
        second
            .put_stored(
                1,
                StoredDocument::with_metadata(fields(1), DocumentMetadata::with_tuple_xmin(11)),
            )
            .unwrap();
        *observed.after_evaluation.lock() = Some(Box::new(move || {
            second
                .put_stored(
                    1,
                    StoredDocument::with_metadata(fields(2), DocumentMetadata::with_tuple_xmin(22)),
                )
                .unwrap();
        }));
        let result = if patch {
            first.patch_fields(1, &fields(3)).map(|_| ())
        } else {
            first.put(1, fields(3))
        };
        assert!(result.is_err());
        assert_eq!(observed.evaluations.load(Ordering::Relaxed), 1);
        inner.rollback_transaction().unwrap();
        assert_eq!(first.get_field(1, "n").unwrap(), Some(Value::Int(2)));
        assert_eq!(
            first.get_metadata(1).unwrap().unwrap().tuple_xmin(),
            Some(22)
        );
    }
}

#[test]
fn compound_document_reads_and_presence_remain_on_one_view() {
    for empty in [false, true] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 20));
        let b = Arc::new(persistence.session(1 << 20));
        let mut second = KeyValueDocumentStore::new(b.clone(), "docs");
        second.put(1, fields(1)).unwrap();
        second.put(2, fields(2)).unwrap();
        second.put(3, fields(3)).unwrap();
        let observed = Arc::new(InterleavedStore::new(a));
        let first = KeyValueDocumentStore::new(observed.clone(), "docs");
        if empty {
            let mut rows = Vec::new();
            first
                .for_each_fields_multi_ref_with_presence(
                    &[1, 2, 3, 4],
                    &[],
                    &mut |id, present, fields| {
                        if id == 1 {
                            second.delete(3).unwrap();
                        }
                        rows.push((id, present, fields.len()));
                        true
                    },
                )
                .unwrap();
            assert_eq!(
                rows,
                [(1, true, 0), (2, true, 0), (3, true, 0), (4, false, 0)]
            );
        } else {
            *observed.after_second_point.lock() = Some(Box::new(move || {
                second.put(3, fields(30)).unwrap();
            }));
            let rows = first.get_fields_multi(&[1, 2, 3], &["n"]).unwrap();
            assert_eq!(rows[&3], vec![Value::Int(3)]);
            assert_eq!(first.get_field(3, "n").unwrap(), Some(Value::Int(30)));
        }
    }
}

#[test]
fn exact_storage_names_remain_distinct_through_every_rename_shape() {
    use uqa_storage::document_store::identifiers::DocumentIdAllocator;
    for source_qualified in [false, true] {
        for target_qualified in [false, true] {
            let persistence = Persistence::new();
            let store = Arc::new(persistence.session(1 << 20));
            let catalog = KeyValueCatalog::new(store.clone());
            catalog.save_schema("public").unwrap();
            let from = if source_qualified {
                "public.source"
            } else {
                "source"
            };
            let to = if target_qualified {
                "public.target"
            } else {
                "target"
            };
            let mut source = KeyValueDocumentStore::new(store.clone(), from);
            source.put(200, fields(200)).unwrap();
            source.delete(200).unwrap();
            source.put(1, fields(1)).unwrap();
            if !source_qualified {
                KeyValueDocumentStore::new(store.clone(), "public.source")
                    .put(2, fields(2))
                    .unwrap();
            }
            KeyValueDocumentStore::new(store.clone(), "public.target")
                .put(3, fields(3))
                .unwrap();
            catalog.save_table(&schema("source")).unwrap();
            let old = catalog.load_tables().unwrap().remove(0);
            catalog.rename_table_data(from, to).unwrap();
            let renamed = catalog.load_tables().unwrap().remove(0);
            assert_eq!(renamed.relation.name, "target");
            assert_eq!(
                (renamed.object_id, renamed.storage_generation),
                (old.object_id, old.storage_generation)
            );
            let target = KeyValueDocumentStore::new(store.clone(), to);
            assert_eq!(target.get_field(1, "n").unwrap(), Some(Value::Int(1)));
            assert!(source.get(1).unwrap().is_none());
            if !source_qualified {
                assert!(KeyValueDocumentStore::new(store.clone(), "public.source")
                    .get(2)
                    .unwrap()
                    .is_some());
            }
            if target_qualified {
                assert_eq!(
                    DocumentIdAllocator::new(
                        store.identifier_allocator(),
                        renamed.object_id,
                        renamed.storage_generation
                    )
                    .unwrap()
                    .allocate(&mut 1)
                    .unwrap(),
                    201
                );
            } else {
                catalog.save_table(&schema("target")).unwrap();
                // Adopt the raw destination through a later rename; its removed high identity must still be reserved.
                catalog.rename_table_data("target", "public.final").unwrap();
                let final_schema = catalog.load_tables().unwrap().remove(0);
                assert_eq!(
                    DocumentIdAllocator::new(
                        store.identifier_allocator(),
                        final_schema.object_id,
                        final_schema.storage_generation
                    )
                    .unwrap()
                    .allocate(&mut 1)
                    .unwrap(),
                    201
                );
            }
        }
    }
}

#[test]
fn destination_collisions_and_duplicate_identity_claims_are_atomic() {
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    catalog.save_table(&schema("source")).unwrap();
    let mut first = KeyValueDocumentStore::new(store.clone(), "public.source");
    let mut second = KeyValueDocumentStore::new(store.clone(), "public.target");
    first.put(1, fields(1)).unwrap();
    second.put(1, fields(2)).unwrap();
    assert!(catalog
        .rename_table_data("public.source", "public.target")
        .is_err());
    assert_eq!(first.get_field(1, "n").unwrap(), Some(Value::Int(1)));
    assert_eq!(second.get_field(1, "n").unwrap(), Some(Value::Int(2)));
    let mut duplicate = catalog.load_tables().unwrap().remove(0);
    duplicate.relation.name = "duplicate".into();
    assert!(catalog.save_table(&duplicate).is_err());
    assert_eq!(catalog.load_tables().unwrap().len(), 1);
    assert!(!store.in_transaction());
}

#[test]
fn failed_identifier_observation_preserves_private_rows_and_table_owner_state() {
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let mut documents = KeyValueDocumentStore::new(store.clone(), "docs");
    store.begin_transaction().unwrap();
    store.put(b"prior", b"kept").unwrap();
    persistence.state.lock().identifier_fault = true;
    assert!(documents.put(99, fields(99)).is_err());
    assert!(documents.is_empty().unwrap());
    assert!(store.scan_prefix(b"O").unwrap().is_empty());
    assert_eq!(
        store.get(b"prior").unwrap().as_deref(),
        Some(b"kept".as_slice())
    );
    persistence.state.lock().identifier_fault = false;
    store.rollback_transaction().unwrap();
}

#[test]
fn legacy_owner_seeding_and_key_reads_do_not_hydrate_large_document_bodies() {
    use uqa_storage::document_store::identifiers::{
        legacy_document_id_metadata_key, DocumentIdAllocator,
    };
    use uqa_storage::MemoryKeyValueStore;
    let memory = Arc::new(MemoryKeyValueStore::new());
    let legacy_catalog = KeyValueCatalog::new(memory.clone());
    legacy_catalog.save_schema("public").unwrap();
    legacy_catalog.save_table(&schema("docs")).unwrap();
    legacy_catalog
        .set_metadata(&legacy_document_id_metadata_key("public.docs"), "1000")
        .unwrap();
    KeyValueDocumentStore::new(memory.clone(), "public.docs")
        .put(
            500,
            BTreeMap::from([("body".into(), Value::Str("x".repeat(1 << 20)))]),
        )
        .unwrap();
    let persistence = Persistence::new();
    let writer = persistence.session(8 << 20);
    for (key, mut value) in memory.scan_prefix(b"").unwrap() {
        // A corrupt body is deliberately unreadable; owner seeding and key cursors need only its identity.
        if key.first() == Some(&b'd') {
            value[0] = 0xff;
        }
        writer.put(&key, &value).unwrap();
    }
    let bounded = Arc::new(persistence.session(32 << 10));
    let mut documents = KeyValueDocumentStore::new(bounded.clone(), "public.docs");
    assert_eq!(documents.doc_ids().unwrap(), vec![500]);
    assert_eq!(documents.len().unwrap(), 1);
    let snapshot = documents.snapshot().unwrap();
    assert_eq!(snapshot.next_doc_ids(None, 1).unwrap(), vec![500]);
    assert!(snapshot.get(500).is_err());
    documents.delete(500).unwrap();
    let catalog = KeyValueCatalog::new(bounded.clone());
    let saved = catalog.load_tables().unwrap().remove(0);
    assert_eq!(
        DocumentIdAllocator::new(
            bounded.identifier_allocator(),
            saved.object_id,
            saved.storage_generation
        )
        .unwrap()
        .allocate(&mut 1)
        .unwrap(),
        1000
    );
    assert_eq!(
        catalog
            .get_metadata(&legacy_document_id_metadata_key("public.docs"))
            .unwrap()
            .as_deref(),
        Some("")
    );
    assert_eq!(snapshot.doc_ids().unwrap(), vec![500]);
}

#[test]
fn mismatched_owner_bindings_reject_data_and_catalog_mutations_without_repairing_corruption() {
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    catalog.save_table(&schema("docs")).unwrap();
    let saved = catalog.load_tables().unwrap().remove(0);
    let mut reverse = vec![b'O', 2];
    reverse.extend_from_slice(&saved.object_id);
    store.put(&reverse, b"somewhere_else").unwrap();
    let before = store.scan_prefix(b"").unwrap();
    let mut documents = KeyValueDocumentStore::new(store.clone(), "public.docs");
    assert!(documents.put(1, fields(1)).is_err());
    assert!(catalog.load_tables().is_err());
    assert!(catalog.save_table(&saved).is_err());
    assert!(catalog.drop_table_and_data("docs").is_err());
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
}

#[test]
fn identifier_inheritance_obeys_batch_order_and_survives_private_undo() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    store.begin_transaction().unwrap();
    store.savepoint("before-transfer").unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.observe_identifier(b"from", 100)?;
            batch.inherit_identifiers(b"from", b"to")?;
            batch.observe_identifier(b"to", 200)?;
            batch.inherit_identifiers(b"to", b"final")?;
            batch.put(b"row", b"private")
        })
        .unwrap();
    store.rollback_to_savepoint("before-transfer").unwrap();
    store.rollback_transaction().unwrap();
    assert!(store.get(b"row").unwrap().is_none());
    for (namespace, expected) in [
        (b"from".as_slice(), 100),
        (b"to".as_slice(), 200),
        (b"final".as_slice(), 200),
    ] {
        assert_eq!(
            store
                .allocate_identifiers(namespace, IdentifierRequest::Observe(0))
                .unwrap()
                .watermark(),
            expected
        );
    }
}

#[test]
fn a_new_generation_seeds_live_ids_and_can_restart_after_rows_are_removed() {
    use uqa_storage::document_store::identifiers::DocumentIdAllocator;
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 20));
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    catalog.save_table(&schema("docs")).unwrap();
    let original = catalog.load_tables().unwrap().remove(0);
    let mut documents = KeyValueDocumentStore::new(store.clone(), "public.docs");
    documents.put(50, fields(50)).unwrap();
    documents.put(100, fields(100)).unwrap();
    documents.delete(100).unwrap();
    catalog
        .set_metadata(
            &uqa_storage::document_store::identifiers::legacy_document_id_metadata_key(
                "public.docs",
            ),
            "1000",
        )
        .unwrap();
    let mut next = original.clone();
    next.storage_generation = [73; 16];
    catalog.save_table(&next).unwrap();
    let allocate = |row: &TableSchema| {
        DocumentIdAllocator::new(
            store.identifier_allocator(),
            row.object_id,
            row.storage_generation,
        )
        .unwrap()
        .allocate(&mut 1)
        .unwrap()
    };
    assert_eq!(allocate(&next), 51);
    documents.clear().unwrap();
    next.storage_generation = [74; 16];
    catalog.save_table(&next).unwrap();
    assert_eq!(allocate(&next), 1);
    assert_eq!(allocate(&original), 1000);
}
