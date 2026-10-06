//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently captured sequence-default input, identity and dependency behavior.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::Expr;

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

fn assert_stored_identity(engine: &Engine, columns: &[&str]) {
    let oid = engine
        .sql("SELECT 'dr_left.original_ids'::regclass::oid AS oid", &[])
        .unwrap()
        .rows[0]["oid"]
        .clone();
    for column in columns {
        let expression = engine
            .column_default_expr("dr_left.defaults", column)
            .unwrap()
            .unwrap();
        let Expr::Func { args, .. } = expression else {
            panic!("stored sequence call");
        };
        assert_eq!(
            args[0],
            Expr::TypedLiteral {
                value: oid.clone(),
                ty: "regclass".into()
            }
        );
        assert!(matches!(oid, Value::Int(_)));
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sequence_default_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("sequence-default-inputs.db");
    let mut engine = open(provider, &path);
    let mut prefix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/sequence_default_input_oracle.expected.json"
    ))
    .unwrap();
    let mut suffix = prefix.clone();
    suffix["cases"] = prefix["cases"].as_array_mut().unwrap().split_off(10).into();
    crate::pg18_oracle::verify(&engine, &prefix.to_string());
    assert_stored_identity(&engine, &["a", "b", "c"]);
    if provider != 0 {
        // Reopen after rename and name reuse, before the first sequence evaluation.
        drop(engine);
        engine = open(provider, &path);
        engine
            .sql("SET search_path=dr_right,dr_left,public", &[])
            .unwrap();
        assert_stored_identity(&engine, &["a", "b", "c"]);
    }
    crate::pg18_oracle::verify(&engine, &suffix.to_string());
    assert_stored_identity(&engine, &["a", "b", "c", "added"]);
    if provider != 0 {
        drop(engine);
        engine = open(provider, &path);
        engine
            .sql("SET search_path=dr_right,dr_left,public", &[])
            .unwrap();
        assert_stored_identity(&engine, &["a", "b", "c", "added"]);
        suffix["cases"].as_array_mut().unwrap().retain(|case| {
            matches!(
                case["id"].as_str().unwrap(),
                "rollback_deparse"
                    | "alter_dependency"
                    | "late_deparse"
                    | "late_has_no_sequence_dependency"
                    | "wrong_kind_runtime"
                    | "shadow_has_no_sequence_dependency"
                    | "shadow_insert"
            )
        });
        crate::pg18_oracle::verify(&engine, &suffix.to_string());
    }
}
