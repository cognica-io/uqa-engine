//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public concatenation identity and retained catalog observations through the provider adapters.

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
fn concatenation_keeps_operator_identity_and_named_routines_across_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concatenation.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("concatenation.json")).unwrap();
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
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn concatenation_syntax_and_registered_callback_keep_separate_definitions(#[case] provider: usize) {
    use uqa_core::Value;
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("concat-callback.db"));
    engine
        .register_scalar_function("concat_op", |args: &[Value]| {
            assert_eq!(args, [Value::Str("a".into()), Value::Str("b".into())]);
            Ok(Value::Str("callback".into()))
        })
        .unwrap();
    engine
        .sql(
            "CREATE VIEW native_concat AS SELECT 'a' || 'b' AS value",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE VIEW callback_concat AS SELECT concat_op('a', 'b') AS value",
            &[],
        )
        .unwrap();
    let session = (provider != 0).then(|| engine.new_session().unwrap());
    for reader in std::iter::once(&engine).chain(session.as_ref()) {
        for (view, expected) in [("native_concat", "ab"), ("callback_concat", "callback")] {
            let result = reader.sql(&format!("SELECT * FROM {view}"), &[]).unwrap();
            assert_eq!(result.value_at(0, 0), Some(&Value::Str(expected.into())));
            for pretty in [false, true] {
                let definition = reader
                    .sql(
                        &format!("SELECT pg_get_viewdef('{view}'::regclass,{pretty})"),
                        &[],
                    )
                    .unwrap();
                let Some(Value::Str(definition)) = definition.value_at(0, 0) else {
                    panic!("view definition")
                };
                let result = reader.sql(definition, &[]).unwrap();
                assert_eq!(result.value_at(0, 0), Some(&Value::Str(expected.into())));
            }
        }
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn previous_native_concatenation_views_restore_identity_once(#[case] provider: usize) {
    use std::sync::Arc;
    use uqa_storage::PersistentStorageProvider;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concat-legacy.db");
    let factory: Arc<dyn PersistentStorageProvider> = match provider {
        1 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )),
        2 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap()),
        3 => Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()),
        _ => unreachable!(),
    };
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    engine
        .sql("CREATE TABLE concat_source(a text, b integer)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO concat_source VALUES ('x', 7)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE VIEW legacy_concat AS SELECT a || b AS value FROM concat_source",
            &[],
        )
        .unwrap();
    drop(engine);
    {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        let mut row = session.catalog.load_views().unwrap().remove(0);
        let mut view: uqa_sql::catalog::stored_view::StoredViewDefinition =
            serde_json::from_str(&row.definition_json).unwrap();
        let mut changed = 0;
        view.query.rewrite_scalar_expressions(&mut |expression| {
            let uqa_sql::ScalarExpr::Func { name, binding, .. } = expression else {
                return;
            };
            if binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .is_some_and(uqa_sql::ast::FunctionDispatch::is_concatenation)
            {
                *binding = None;
                *name = "concat_op".into();
                changed += 1;
            }
        });
        assert_eq!(changed, 1);
        row.definition_json = serde_json::to_string(&view).unwrap();
        session.catalog.save_view(&row).unwrap();
        session.backend.commit_transaction().unwrap();
    }
    let mut previous = None;
    for _ in 0..2 {
        let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let result = engine.sql("SELECT * FROM legacy_concat", &[]).unwrap();
        assert_eq!(
            result.value_at(0, 0),
            Some(&uqa_core::Value::Str("x7".into()))
        );
        drop(engine);
        let session = factory.open_session().unwrap();
        let definition = session
            .catalog
            .load_views()
            .unwrap()
            .remove(0)
            .definition_json;
        assert!(definition.contains("Concat"));
        if let Some(previous) = &previous {
            assert_eq!(previous, &definition);
        }
        previous = Some(definition);
    }
}
