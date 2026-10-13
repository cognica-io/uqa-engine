//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured pattern operator identity, input typing and byte semantics.

use uqa_engine::Engine;

#[path = "pattern_operators/restoration.rs"]
mod restoration;

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
fn pattern_operator_identity_and_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("pattern.db"));
    crate::pg18_oracle::verify(&engine, include_str!("pattern_operators.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn pattern_escape_helpers_and_null_evaluation_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("pattern-helpers.db"));
    crate::pg18_oracle::verify(&engine, include_str!("pattern_helpers.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn pattern_and_ordinary_call_negation_preserve_postgresql_view_definition(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("pattern-negation.db"));
    crate::pg18_oracle::verify(&engine, include_str!("pattern_negation.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn pattern_operator_views_and_predicates_match_postgresql_across_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pattern-views.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pattern_views.json")).unwrap();
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
fn native_patterns_and_registered_callbacks_keep_separate_definitions(#[case] provider: usize) {
    use uqa_core::Value;
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("pattern-callback.db"));
    for name in ["like", "ilike", "regex_match_op", "regex_imatch_op"] {
        engine
            .register_scalar_function(name, |_: &[Value]| Ok(Value::Bool(false)))
            .unwrap();
    }
    engine
        .sql(
            "CREATE VIEW native_patterns AS SELECT 'a' LIKE 'a' AS l, 'a' ILIKE 'A' AS il, 'a' ~ 'a' AS r, 'a' ~* 'A' AS ri",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE VIEW callback_patterns AS SELECT like('a','a') AS l, ilike('a','A') AS il, regex_match_op('a','a') AS r, regex_imatch_op('a','A') AS ri",
            &[],
        )
        .unwrap();
    let session = (provider != 0).then(|| engine.new_session().unwrap());
    for reader in std::iter::once(&engine).chain(session.as_ref()) {
        for (view, expected) in [("native_patterns", true), ("callback_patterns", false)] {
            let result = reader.sql(&format!("SELECT * FROM {view}"), &[]).unwrap();
            for column in 0..4 {
                assert_eq!(result.value_at(0, column), Some(&Value::Bool(expected)));
            }
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
                for column in 0..4 {
                    assert_eq!(result.value_at(0, column), Some(&Value::Bool(expected)));
                }
            }
        }
    }
}
