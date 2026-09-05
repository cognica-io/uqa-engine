//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::Value;

use super::super::codec::{document_key, encode_value, single_str_key};
use super::super::{
    KeyValueCatalog, KeyValueDocumentStore, DOCUMENT_VALUE_V1_PREFIX, DOCUMENT_VALUE_V2_PREFIX,
    TAG_METADATA,
};
use super::store;
use crate::catalog::{CatalogFacade, TableSchema};
use crate::document_store::DocumentStore;
use crate::{DocumentMetadata, StoredDocument};

#[test]
fn key_value_document_store_round_trips_tuple_metadata_outside_user_fields() {
    let store = store();
    let mut docs = KeyValueDocumentStore::new(Arc::clone(&store), "articles");
    docs.put_stored(
        9,
        StoredDocument::with_metadata(
            BTreeMap::from([
                ("title".into(), Value::Str("Rust".into())),
                ("missing".into(), Value::Null),
            ]),
            DocumentMetadata::with_tuple_xmin(41),
        ),
    )
    .unwrap();

    let stored = docs.get_stored(9).unwrap().unwrap();
    assert_eq!(stored.metadata().tuple_xmin(), Some(41));
    assert_eq!(
        stored.fields(),
        &BTreeMap::from([("title".into(), Value::Str("Rust".into()))])
    );
    assert!(!stored.fields().contains_key("\0uqa.system.xmin"));
    docs.put(
        9,
        BTreeMap::from([("title".into(), Value::Str("Engine".into()))]),
    )
    .unwrap();
    assert_eq!(
        docs.get_stored(9).unwrap().unwrap().metadata().tuple_xmin(),
        Some(41)
    );
}

#[test]
fn key_value_document_migration_separates_xmin_and_preserves_user_collisions() {
    let store = store();
    let catalog = KeyValueCatalog::new(Arc::clone(&store));
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "system_xmin"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: r#"[{"name":"value"}]"#.into(),
            constraints_json: String::new(),
        })
        .unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "user_xmin"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [3; 16],
            storage_generation: [4; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: r#"[{"name":"xmin"}]"#.into(),
            constraints_json: String::new(),
        })
        .unwrap();
    catalog
        .save_table(&TableSchema {
            relation: crate::catalog::RelationIdentity::new("public", "schemaless_user_xmin"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [5; 16],
            storage_generation: [6; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    for (table, xmin, user_marker) in [
        ("public.system_xmin", 51, false),
        ("public.user_xmin", 52, false),
        ("public.schemaless_user_xmin", 53, true),
    ] {
        let mut document: crate::document_store::Document = BTreeMap::from([
            ("\0uqa.system.xmin".into(), Value::Int(xmin)),
            ("xmin".into(), Value::Int(xmin)),
        ]);
        if user_marker {
            document.insert("\0uqa.user.xmin".into(), Value::Bool(true));
        }
        let mut encoded = DOCUMENT_VALUE_V1_PREFIX.to_vec();
        encoded.extend(encode_value(&document).unwrap());
        store
            .put(&document_key(table, 1).unwrap(), &encoded)
            .unwrap();
    }

    KeyValueDocumentStore::migrate_legacy_storage(store.as_ref()).unwrap();

    let system = KeyValueDocumentStore::new(Arc::clone(&store), "public.system_xmin")
        .get_stored(1)
        .unwrap()
        .unwrap();
    assert!(system.fields().is_empty());
    assert_eq!(system.metadata().tuple_xmin(), Some(51));
    let user = KeyValueDocumentStore::new(Arc::clone(&store), "public.user_xmin")
        .get_stored(1)
        .unwrap()
        .unwrap();
    assert_eq!(user.fields().get("xmin"), Some(&Value::Int(52)));
    assert_eq!(user.metadata().tuple_xmin(), Some(52));
    let schemaless_user =
        KeyValueDocumentStore::new(Arc::clone(&store), "public.schemaless_user_xmin")
            .get_stored(1)
            .unwrap()
            .unwrap();
    assert_eq!(schemaless_user.fields().get("xmin"), Some(&Value::Int(53)));
    assert_eq!(schemaless_user.metadata().tuple_xmin(), Some(53));
    for table in [
        "public.system_xmin",
        "public.user_xmin",
        "public.schemaless_user_xmin",
    ] {
        assert!(store
            .get(&document_key(table, 1).unwrap())
            .unwrap()
            .unwrap()
            .starts_with(DOCUMENT_VALUE_V2_PREFIX));
    }
}

#[test]
fn key_value_document_migration_rolls_back_every_record_on_invalid_metadata() {
    let store = store();
    for (doc_id, xmin) in [(1, Value::Int(61)), (2, Value::Str("invalid".into()))] {
        let document: crate::document_store::Document = BTreeMap::from([
            ("\0uqa.system.xmin".into(), xmin.clone()),
            ("xmin".into(), xmin),
        ]);
        let mut encoded = DOCUMENT_VALUE_V1_PREFIX.to_vec();
        encoded.extend(encode_value(&document).unwrap());
        store
            .put(
                &document_key("public.invalid_xmin", doc_id).unwrap(),
                &encoded,
            )
            .unwrap();
    }

    let error = KeyValueDocumentStore::migrate_legacy_storage(store.as_ref()).unwrap_err();
    assert!(error
        .to_string()
        .contains("legacy tuple xmin is not an integer"));
    for doc_id in [1, 2] {
        assert!(store
            .get(&document_key("public.invalid_xmin", doc_id).unwrap())
            .unwrap()
            .unwrap()
            .starts_with(DOCUMENT_VALUE_V1_PREFIX));
    }
    assert!(store
        .get(&single_str_key(TAG_METADATA, "document_storage_format").unwrap())
        .unwrap()
        .is_none());
    assert!(!store.in_transaction());
}
