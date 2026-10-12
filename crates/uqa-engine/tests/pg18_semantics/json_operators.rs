//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn json_operators_match_postgresql_inputs_definitions_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("json-operators.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("json_operators.json")).unwrap();
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
    for case in initial["cases"].as_array().unwrap() {
        let Some(sql) = case["sql"]
            .as_str()
            .filter(|sql| sql.starts_with("CREATE VIEW "))
        else {
            continue;
        };
        let name = sql.split_whitespace().nth(2).unwrap();
        let expected = initial["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["sql"] == format!("SELECT * FROM {name}"))
            .unwrap();
        for pretty in [false, true] {
            let definition = engine
                .sql(
                    &format!("SELECT pg_get_viewdef('{name}'::regclass, {pretty})"),
                    &[],
                )
                .unwrap();
            let Some(uqa_core::Value::Str(definition)) = definition.value_at(0, 0) else {
                panic!("view SQL")
            };
            let recreated = format!("reconstructed_{name}_{pretty}");
            engine
                .sql(&format!("CREATE VIEW {recreated} AS {definition}"), &[])
                .unwrap();
            let mut expected = expected.clone();
            expected["sql"] = format!("SELECT * FROM {recreated}").into();
            crate::pg18_oracle::verify(
                &engine,
                &serde_json::json!({"postgresql_version": initial["postgresql_version"], "cases": [expected]}).to_string(),
            );
        }
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn legacy_json_path_views_restore_selected_arrays_once(#[case] provider: usize) {
    use std::sync::Arc;
    use uqa_storage::PersistentStorageProvider;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-json-path.db");
    let factory: Arc<dyn PersistentStorageProvider> = match provider {
        1 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )),
        2 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap()),
        3 => Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()),
        _ => unreachable!(),
    };
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("json_operators.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        if sql.starts_with("CREATE TABLE documents")
            || sql.starts_with("INSERT INTO documents")
            || sql.starts_with("CREATE VIEW json_function ")
        {
            engine.sql(sql, &[]).unwrap();
        }
    }
    drop(engine);
    {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        let mut row = session.catalog.load_views().unwrap().remove(0);
        let mut view: uqa_sql::catalog::stored_view::StoredViewDefinition =
            serde_json::from_str(&row.definition_json).unwrap();
        let mut changed = 0;
        view.query.rewrite_scalar_expressions(&mut |expression| {
            let uqa_sql::ScalarExpr::Func { binding, args, .. } = expression else {
                return;
            };
            if !matches!(
                binding.as_ref().and_then(|binding| binding.dispatch),
                Some(uqa_sql::ast::FunctionDispatch::JsonPath { .. })
            ) {
                return;
            }
            let uqa_sql::ScalarExpr::Array(paths) = args.pop().unwrap() else {
                panic!("selected native path array")
            };
            args.extend(paths);
            *binding = None;
            changed += 1;
        });
        assert_eq!(changed, 1);
        row.definition_json = serde_json::to_string(&view).unwrap();
        session.catalog.save_view(&row).unwrap();
        session.backend.commit_transaction().unwrap();
    }
    let mut observations = reference;
    observations["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| {
            case["sql"].as_str().is_some_and(|sql| {
                sql == "SELECT * FROM json_function"
                    || sql.starts_with("SELECT pg_get_viewdef('json_function'::regclass,")
            }) && case["reopen"] != true
        });
    assert_eq!(observations["cases"].as_array().unwrap().len(), 2);
    let mut previous = None;
    for _ in 0..2 {
        let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        crate::pg18_oracle::verify(&engine, &observations.to_string());
        drop(engine);
        let session = factory.open_session().unwrap();
        let definition = session
            .catalog
            .load_views()
            .unwrap()
            .remove(0)
            .definition_json;
        assert!(definition.contains("JsonPath"));
        if let Some(previous) = &previous {
            assert_eq!(previous, &definition);
        }
        previous = Some(definition);
    }
}
