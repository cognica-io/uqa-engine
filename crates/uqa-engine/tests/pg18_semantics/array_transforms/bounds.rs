//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::named_overloads::open;
use std::sync::Arc;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn array_bounds_match_postgresql_values_types_effects_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("array-bounds.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!("bounds.json")).unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn legacy_array_slice_bounds_upgrade_once_before_folding(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-array-bounds.db");
    let factory: Arc<dyn PersistentStorageProvider> = match provider {
        1 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )),
        2 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap()),
        3 => Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()),
        _ => unreachable!(),
    };
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    for sql in [
        "CREATE TABLE array_bound_values(a integer[])",
        "INSERT INTO array_bound_values VALUES (ARRAY[1,2,3])",
        "CREATE VIEW legacy_bounds AS SELECT a[:2]::text AS omitted, a[NULL::integer:2]::text AS typed, a[CASE WHEN true THEN NULL::integer ELSE 1 END:2]::text AS computed FROM array_bound_values",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    drop(engine);
    {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        let mut view = session.catalog.load_views().unwrap().remove(0);
        let mut definition: serde_json::Value =
            serde_json::from_str(&view.definition_json).unwrap();
        assert_eq!(replace_slice_dispatches(&mut definition), 3);
        view.definition_json = definition.to_string();
        session.catalog.save_view(&view).unwrap();
        session.backend.commit_transaction().unwrap();
    }
    let mut persisted = None;
    for _ in 0..2 {
        let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let result = engine.sql("SELECT * FROM legacy_bounds", &[]).unwrap();
        // The three endpoint values are independently captured in bounds.json.
        assert_eq!(result.rows[0]["omitted"], Value::Str("{1,2}".into()));
        assert_eq!(result.rows[0]["typed"], Value::Null);
        assert_eq!(result.rows[0]["computed"], Value::Null);
        drop(engine);
        let session = factory.open_session().unwrap();
        let definition = session
            .catalog
            .load_views()
            .unwrap()
            .remove(0)
            .definition_json;
        assert!(definition.contains("ArraySlicesWithBounds"));
        assert!(!definition.contains("\"ArraySlices\""));
        if let Some(previous) = &persisted {
            assert_eq!(previous, &definition);
        }
        persisted = Some(definition);
    }
}

fn replace_slice_dispatches(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().map(replace_slice_dispatches).sum(),
        serde_json::Value::Object(fields) => {
            if fields.contains_key("ArraySlicesWithBounds") {
                *value = serde_json::Value::String("ArraySlices".into());
                1
            } else {
                fields.values_mut().map(replace_slice_dispatches).sum()
            }
        }
        _ => 0,
    }
}
