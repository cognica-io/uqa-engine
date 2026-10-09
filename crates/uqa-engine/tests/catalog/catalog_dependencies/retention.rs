//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::Value;

fn dependency_rows(engine: &Engine, relation: &str) -> Vec<uqa_sql::ResultRow> {
    engine.sql(&format!(
        "SELECT pg_describe_object(a.classid,a.objid,a.objsubid) AS object, pg_describe_object(a.refclassid,a.refobjid,a.refobjsubid) AS referenced \
         FROM pg_depend a JOIN pg_depend b ON a.classid=b.classid AND a.objid=b.objid AND a.objsubid=b.objsubid AND a.refclassid=b.refclassid AND a.refobjid=b.refobjid AND a.refobjsubid=b.refobjsubid AND a.deptype=b.deptype \
         WHERE a.classid='pg_class'::regclass AND a.objid='{relation}'::regclass AND a.refclassid='pg_type'::regclass"
    ), &[]).unwrap().rows
}

#[test]
fn dependency_aliases_and_descriptions_follow_private_ddl_and_rollback() {
    let directory = tempfile::tempdir().unwrap();
    for provider in 0..4 {
        let path = directory.path().join(format!("dependencies-{provider}.db"));
        let engine = match provider {
            0 => Engine::new(),
            1 => Engine::open(&path).unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
        };
        engine.sql("CREATE SCHEMA dep_cache; CREATE DOMAIN dep_cache.amount AS integer; CREATE TABLE dep_cache.items(id dep_cache.amount)", &[]).unwrap();
        let expected = dependency_rows(&engine, "dep_cache.items");
        // PostgreSQL 18.4 returns one dependency in this self join, with these exact descriptions.
        assert_eq!(expected.len(), 1);
        assert_eq!(
            expected[0]["object"],
            Value::Str("column id of table dep_cache.items".into())
        );
        assert_eq!(
            expected[0]["referenced"],
            Value::Str("type dep_cache.amount".into())
        );
        engine
            .sql(
                "BEGIN; SAVEPOINT kept; ALTER TABLE dep_cache.items RENAME TO renamed",
                &[],
            )
            .unwrap();
        let renamed = dependency_rows(&engine, "dep_cache.renamed");
        assert_eq!(renamed.len(), 1);
        assert_eq!(
            renamed[0]["object"],
            Value::Str("column id of table dep_cache.renamed".into())
        );
        assert_eq!(renamed[0]["referenced"], expected[0]["referenced"]);
        engine.sql("ROLLBACK TO kept; COMMIT", &[]).unwrap();
        assert_eq!(dependency_rows(&engine, "dep_cache.items"), expected);
        let oid = engine
            .sql("SELECT 'dep_cache.items'::regclass::oid AS oid", &[])
            .unwrap()
            .rows[0]["oid"]
            .clone();
        let Value::Int(oid) = oid else {
            panic!("relation oid");
        };
        engine.sql("DROP TABLE dep_cache.items", &[]).unwrap();
        let remaining = engine.sql(&format!("SELECT count(*) AS n FROM pg_depend WHERE classid='pg_class'::regclass AND objid={oid}"), &[]).unwrap();
        assert_eq!(remaining.rows[0]["n"], Value::Int(0));
    }
}
