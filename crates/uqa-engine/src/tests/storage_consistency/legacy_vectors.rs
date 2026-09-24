//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial-open conversion of predecessor typed rows is atomic with indexes and statistics.

use crate::Engine;
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};
use uqa_core::{ArrayValue, DocId, Value};
use uqa_storage::{
    catalog::ColumnStatsRow, CatalogFacade, PersistentStorageBackend, StorageBackendError,
    StorageBackendResult, StoredDocument, ValueIndexEntry, ValueIndexKey,
};

const TABLE: &str = "public.vector_restore";
const VERSION: &str = "sql_legacy_vector_carrier_version";

fn open(provider: usize, path: &Path) -> StorageBackendResult<Engine> {
    let engine = match provider {
        0 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteStorageProvider::new(
                uqa_storage_sqlite::ManagedConnection::open(path).unwrap(),
            ),
        )),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        )),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        )),
        _ => unreachable!(),
    }?;
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    Ok(engine)
}

fn setup(engine: &Engine) {
    engine.sql(
        "CREATE SEQUENCE vector_checks;
         CREATE DOMAIN restored_vector AS int2vector CHECK(nextval('vector_checks') > 0);
         CREATE TABLE vector_restore(id integer PRIMARY KEY, k restored_vector UNIQUE, o oidvector, a integer[], nested int2vector[]);
         INSERT INTO vector_restore VALUES
           (1, '1 2', '4294967295 0', '[0:1]={7,8}', ARRAY['1 2'::int2vector,'3 4'::int2vector]),
           (2, '3 4', '', '[0:1]={9,10}', ARRAY['5 6'::int2vector,'7 8'::int2vector]);
         CREATE INDEX vector_expression ON vector_restore ((k::int2vector)) WHERE id > 0;
         ANALYZE vector_restore;", &[],
    ).unwrap();
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    rows: Vec<(DocId, StoredDocument)>,
    indexes: Vec<(ValueIndexKey, Vec<ValueIndexEntry>)>,
    statistics: Vec<ColumnStatsRow>,
    definitions: Vec<serde_json::Value>,
}

fn snapshot(backend: &dyn PersistentStorageBackend, catalog: &dyn CatalogFacade) -> Snapshot {
    let documents = backend.document_store(TABLE);
    let ids = documents.doc_ids().unwrap();
    Snapshot {
        rows: ids
            .iter()
            .map(|id| (*id, documents.get_stored(*id).unwrap().unwrap()))
            .collect(),
        indexes: backend
            .btree_index_fields(TABLE)
            .unwrap()
            .into_iter()
            .map(|key| {
                let values = ids
                    .iter()
                    .map(|id| backend.read_btree_index_entry(TABLE, &key, *id).unwrap())
                    .collect();
                (key, values)
            })
            .collect(),
        statistics: catalog.load_column_stats(TABLE).unwrap(),
        definitions: catalog
            .load_catalog_indexes()
            .unwrap()
            .into_iter()
            .map(|index| {
                serde_json::json!({
                    "name":index.relation.qualified_name(), "table":index.table_name,
                    "columns":index.columns_json, "definition":index.definition_json,
                })
            })
            .collect(),
    }
}

fn predecessor_vector(value: &Value, array: bool) -> Value {
    let Value::LegacyVector(vector) = value else {
        panic!("expected a catalog vector")
    };
    if array {
        Value::Array(ArrayValue::try_new(vector.elements().to_vec()).unwrap())
    } else {
        Value::List(vector.elements().to_vec())
    }
}

fn seed_predecessor_carriers(engine: &Engine, duplicates: bool) {
    let backend = engine.storage.backend.as_ref().unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    backend.begin_transaction().unwrap();
    let mut documents = backend.document_store(TABLE);
    let ids = documents.doc_ids().unwrap();
    for (ordinal, id) in ids.iter().enumerate() {
        let mut row = documents.get_stored(*id).unwrap().unwrap();
        let fields = row.fields_mut();
        fields.insert(
            "k".into(),
            if duplicates && ordinal == 1 {
                Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap())
            } else {
                predecessor_vector(&fields["k"], ordinal == 1)
            },
        );
        fields.insert("o".into(), predecessor_vector(&fields["o"], true));
        let Value::Array(nested) = &fields["nested"] else {
            panic!("expected an array")
        };
        let old_nested = nested
            .elements()
            .iter()
            .map(|value| predecessor_vector(value, false))
            .collect();
        fields.insert(
            "nested".into(),
            Value::Array(ArrayValue::try_new(old_nested).unwrap()),
        );
        documents.put_stored(*id, row).unwrap();
    }
    for key in backend.btree_index_fields(TABLE).unwrap() {
        let values = ids
            .iter()
            .map(|id| {
                let row = documents.get_stored(*id).unwrap().unwrap();
                let value = match &key {
                    ValueIndexKey::Column(column) => row.fields()[column].clone(),
                    ValueIndexKey::Index(_) => Value::Row(vec![row.fields()["k"].clone()]),
                };
                (*id, value)
            })
            .collect::<Vec<_>>();
        backend.replace_btree_index(TABLE, &key, &values).unwrap();
    }
    catalog.delete_metadata(VERSION).unwrap();
    backend.commit_transaction().unwrap();
}

#[rstest::rstest]
fn predecessor_vectors_reopen_with_original_tuple_and_index_identities(
    #[values(0, 1, 2)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vectors.db");
    let engine = open(provider, &path).unwrap();
    setup(&engine);
    let before = snapshot(
        engine.storage.backend.as_ref().unwrap().as_ref(),
        engine.storage.catalog.as_ref().unwrap().as_ref(),
    );
    let checks = engine
        .sql("SELECT last_value FROM vector_checks", &[])
        .unwrap()
        .rows;
    seed_predecessor_carriers(&engine, false);
    drop(engine);
    let restored = open(provider, &path).unwrap();
    let after = snapshot(
        restored.storage.backend.as_ref().unwrap().as_ref(),
        restored.storage.catalog.as_ref().unwrap().as_ref(),
    );
    assert_eq!(after.rows, before.rows);
    assert_eq!(after.indexes, before.indexes);
    assert_eq!(after.definitions, before.definitions);
    assert!(after.statistics.is_empty());
    assert_eq!(
        restored
            .sql("SELECT last_value FROM vector_checks", &[])
            .unwrap()
            .rows,
        checks
    );
    assert_eq!(
        restored
            .sql(
                "SELECT id FROM vector_restore WHERE k='1 2'::restored_vector",
                &[]
            )
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        restored
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .get_metadata(VERSION)
            .unwrap()
            .as_deref(),
        Some("1")
    );
    drop(restored);
    let reopened = open(provider, &path).unwrap();
    assert_eq!(
        snapshot(
            reopened.storage.backend.as_ref().unwrap().as_ref(),
            reopened.storage.catalog.as_ref().unwrap().as_ref()
        )
        .rows,
        before.rows
    );
}

#[rstest::rstest]
fn duplicate_normalized_keys_roll_back_initial_open(#[values(0, 1, 2)] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("duplicates.db");
    let engine = open(provider, &path).unwrap();
    setup(&engine);
    seed_predecessor_carriers(&engine, true);
    let backend = engine.storage.backend.as_ref().unwrap().clone();
    let catalog = engine.storage.catalog.as_ref().unwrap().clone();
    let before = snapshot(backend.as_ref(), catalog.as_ref());
    let provider = engine.storage.provider.as_ref().unwrap().clone();
    drop(engine);
    let Err(error) = Engine::from_persistent_provider(provider) else {
        panic!("duplicate normalized keys must abort initial restoration");
    };
    let StorageBackendError::Backend { source, .. } = error else {
        panic!("expected the typed unique-index validation error: {error}")
    };
    assert_eq!(
        source
            .downcast_ref::<uqa_sql::SQLError>()
            .unwrap()
            .sqlstate(),
        Some("23505")
    );
    assert_eq!(snapshot(backend.as_ref(), catalog.as_ref()), before);
    assert_eq!(catalog.get_metadata(VERSION).unwrap(), None);
    assert!(!backend.in_transaction());
}
