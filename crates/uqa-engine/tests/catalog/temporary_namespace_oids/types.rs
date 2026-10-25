//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 creates types in the session namespace through both its alias and search path.

use super::{int, run};
use uqa_engine::Engine;

pub(super) fn open(provider: usize, path: &std::path::Path) -> Engine {
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
fn type_creation_and_lifecycle_resolve_the_session_namespace(
    #[values(0, 1, 2, 3)] provider: usize,
    #[values(
        "DOMAIN object AS integer",
        "TYPE object AS ENUM ('ready')",
        "TYPE object AS (n integer)",
        "TYPE object"
    )]
    declaration: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("temporary-types.db"));
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/temporary_type_namespaces.expected.json"
    ))
    .unwrap();
    assert!(reference["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    for explicit in [true, false] {
        let expected = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["declaration"] == declaration && case["explicit"] == explicit)
            .unwrap();
        run(&engine, "BEGIN");
        if !explicit {
            run(&engine, "SET LOCAL search_path=pg_temp,public");
        }
        let declaration = if explicit {
            declaration.replace("object", "pg_temp.object")
        } else {
            declaration.to_owned()
        };
        run(&engine, &format!("CREATE {declaration}"));
        let created = serde_json::json!({
            "allocated": int(&engine, "SELECT pg_my_temp_schema()::bigint") != 0,
            "objects": int(&engine, "SELECT count(*) FROM pg_type WHERE typname='object' AND typnamespace=pg_my_temp_schema()"),
        });
        if declaration.ends_with("object") {
            run(&engine, "CREATE TYPE pg_temp.object AS ENUM ('completed')");
        }
        run(
            &engine,
            "CREATE TEMP TABLE typed_object(v pg_temp.object,a pg_temp.object[])",
        );
        let referenced = serde_json::json!({
            "columns": int(&engine, "SELECT count(*) FROM pg_attribute WHERE attrelid='typed_object'::regclass AND attnum>0 AND atttypid IN ('pg_temp.object'::regtype,'pg_temp.object[]'::regtype)"),
            "scalar": int(&engine, "SELECT (pg_typeof(NULL::pg_temp.object)::oid='pg_temp.object'::regtype::oid)::integer") == 1,
            "array": int(&engine, "SELECT (pg_typeof(NULL::pg_temp.object[])::oid='pg_temp.object[]'::regtype::oid)::integer") == 1,
        });
        run(&engine, "DROP TABLE typed_object");
        run(&engine, "ALTER TYPE pg_temp.object RENAME TO renamed");
        run(&engine, "DROP TYPE pg_temp.renamed");
        let removed = serde_json::json!({
            "objects": int(&engine, "SELECT count(*) FROM pg_type WHERE typname IN ('object','renamed') AND typnamespace=pg_my_temp_schema()"),
        });
        run(&engine, "ROLLBACK");
        let rolled_back = serde_json::json!({
            "allocated": int(&engine, "SELECT pg_my_temp_schema()::bigint") != 0,
        });
        assert_eq!(
            serde_json::json!([created, referenced, removed, rolled_back]),
            expected["observations"]
        );
    }
}
