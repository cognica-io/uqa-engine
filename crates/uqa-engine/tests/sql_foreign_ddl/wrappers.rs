//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::columns::open;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_catalog_rows_and_wrapper_deletion_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("foreign-catalog.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/foreign_catalog_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn native_handler_aliases_select_the_original_adapter_and_keep_their_owner(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-handler.db");
    let engine = open(provider, &path);
    engine.sql("CREATE FOREIGN DATA WRAPPER unused_memory HANDLER memory_fdw_handler; DROP FOREIGN DATA WRAPPER unused_memory", &[]).unwrap();
    engine.sql("CREATE FOREIGN DATA WRAPPER custom_memory HANDLER memory_fdw_handler; CREATE SERVER custom_source FOREIGN DATA WRAPPER custom_memory; CREATE FOREIGN TABLE custom_rows(id integer) SERVER custom_source OPTIONS (z 'last',a 'first'); CREATE ROLE handler_other", &[]).unwrap();
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    engine
        .load_memory_foreign_table(
            "custom_rows",
            vec![super::row(&[("id", uqa_core::Value::Int(7))])],
        )
        .unwrap();
    let result = engine.sql("SELECT id FROM custom_rows", &[]).unwrap();
    assert_eq!(result.rows[0]["id"], uqa_core::Value::Int(7));
    let result = engine.sql("SELECT w.fdwhandler=p.oid AS retained,p.prorettype=3115 AS handler_type,w.fdwacl IS NULL AS owner_only FROM pg_foreign_data_wrapper w JOIN pg_proc p ON p.oid=w.fdwhandler WHERE w.fdwname='custom_memory'", &[]).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert!(result.rows[0]
        .values()
        .all(|value| *value == uqa_core::Value::Bool(true)));
    assert_eq!(
        engine
            .sql(
                "SELECT fdwname FROM pg_foreign_data_wrapper WHERE fdwname='unused_memory'",
                &[]
            )
            .unwrap()
            .rows
            .len(),
        0
    );
    engine.sql("SET ROLE handler_other", &[]).unwrap();
    assert_eq!(
        engine
            .sql(
                "CREATE SERVER forbidden_custom_source FOREIGN DATA WRAPPER custom_memory",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    engine.sql("CREATE SERVER public_native_source FOREIGN DATA WRAPPER memory_fdw; RESET ROLE; DROP FOREIGN DATA WRAPPER custom_memory CASCADE", &[]).unwrap();
    assert!(engine.foreign_table("custom_rows").unwrap().is_none());
}

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
        selected["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| (case["reopen"] == true) == reopen);
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
