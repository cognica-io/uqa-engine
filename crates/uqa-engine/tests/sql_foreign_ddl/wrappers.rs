//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::columns::open;

// These independently captured catalog observations remain the next #523 unit's explicit obligation.
const CATALOG_PROJECTION_CASES: [&str; 7] = [
    "plain_catalog",
    "options_catalog",
    "server_reference",
    "rollback_catalog",
    "validated_server_options",
    "stored_catalog",
    "stored_reference",
];

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn wrapper_declaration_validation_and_dependencies_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wrappers.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/foreign_wrapper_oracle.expected.json"
    ))
    .unwrap();
    let transcript = |reopen| {
        let mut selected = reference.clone();
        selected["cases"].as_array_mut().unwrap().retain(|case| {
            (case["reopen"] == true) == reopen
                && !CATALOG_PROJECTION_CASES.contains(&case["id"].as_str().unwrap())
        });
        selected.to_string()
    };
    crate::pg18_oracle::verify(&engine, &transcript(false));
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    crate::pg18_oracle::verify(&engine, &transcript(true));
    // Aborted creation must leave its name available; successful recreation must survive another opening.
    engine.sql("CREATE FOREIGN DATA WRAPPER wrapper_rollback; CREATE FOREIGN DATA WRAPPER wrapper_rejected", &[]).unwrap();
    engine
        .sql("DROP FUNCTION wrapper_validator(text[],oid) CASCADE", &[])
        .unwrap();
    assert!(engine
        .foreign_table("wrapper_valid_rows")
        .unwrap()
        .is_none());
    let error = engine
        .sql(
            "CREATE SERVER removed_wrapper_source FOREIGN DATA WRAPPER wrapper_valid",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    assert!(engine
        .foreign_table("wrapper_valid_rows")
        .unwrap()
        .is_none());
    let error = engine
        .sql("CREATE FOREIGN DATA WRAPPER wrapper_rollback", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"));
    engine
        .sql("CREATE FOREIGN DATA WRAPPER wrapper_valid", &[])
        .unwrap();
}

#[test]
fn direct_foreign_table_options_keep_map_semantics() {
    let engine = uqa_engine::Engine::new();
    engine
        .sql(
            "CREATE SERVER direct_options FOREIGN DATA WRAPPER memory_fdw",
            &[],
        )
        .unwrap();
    engine
        .register_foreign_table(
            "direct_rows".into(),
            "direct_options".into(),
            vec![super::integer_column("id")],
            vec![
                ("same".into(), "first".into()),
                ("same".into(), "last".into()),
                ("has=equals".into(), "allowed".into()),
            ],
            false,
        )
        .unwrap();
    let table = engine.foreign_table("direct_rows").unwrap().unwrap();
    assert_eq!(table.options["same"], "last");
    assert_eq!(table.options["has=equals"], "allowed");
}
