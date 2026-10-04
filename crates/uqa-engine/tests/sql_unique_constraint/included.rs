//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key constraints whose supporting index includes columns beside the key, with the names, catalog rows and dependencies of `PostgreSQL` 18.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
}

/// The rows of `sql` with every value rendered as text.
fn rows(engine: &Engine, sql: &str) -> Vec<Vec<String>> {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result
        .rows
        .iter()
        .map(|row| {
            result
                .columns
                .iter()
                .map(|column| match &row[column.as_str()] {
                    Value::Str(text) => text.clone(),
                    Value::Null => "NULL".into(),
                    Value::Int(number) => number.to_string(),
                    Value::Bool(flag) => flag.to_string(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

fn create(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE u(a int,b int,c int,UNIQUE(a) INCLUDE(b)); CREATE TABLE x(a int,b int,c int,UNIQUE(a,b) INCLUDE(c),PRIMARY KEY(c) INCLUDE(a)); CREATE TABLE m(a int,b int); ALTER TABLE m ADD CONSTRAINT m_named UNIQUE(a) INCLUDE(b); ALTER TABLE m ADD UNIQUE(a) INCLUDE(b); ALTER TABLE m ADD UNIQUE(b) INCLUDE(a,a)",
    );
}

fn catalog_rows_match_postgresql(engine: &Engine) {
    assert_eq!(
        rows(
            engine,
            "SELECT conname::text,contype::text,conkey::text FROM pg_constraint WHERE conrelid IN('u'::regclass,'x'::regclass,'m'::regclass) AND contype IN('u','p') ORDER BY conname"
        ),
        [
            ["m_a_b_key", "u", "{1}"],
            ["m_b_a_a1_key", "u", "{2}"],
            ["m_named", "u", "{1}"],
            ["u_a_b_key", "u", "{1}"],
            ["x_a_b_c_key", "u", "{1,2}"],
            ["x_pkey", "p", "{3}"],
        ]
    );
    assert_eq!(
        rows(
            engine,
            "SELECT c.relname::text,i.indnatts::text,i.indnkeyatts::text,i.indkey::text,i.indisunique::text,i.indisprimary::text,pg_get_indexdef(i.indexrelid) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid WHERE i.indrelid IN('u'::regclass,'x'::regclass,'m'::regclass) ORDER BY c.relname"
        ),
        [
            ["m_a_b_key", "2", "1", "1 2", "true", "false", "CREATE UNIQUE INDEX m_a_b_key ON public.m USING btree (a) INCLUDE (b)"],
            ["m_b_a_a1_key", "3", "1", "2 1 1", "true", "false", "CREATE UNIQUE INDEX m_b_a_a1_key ON public.m USING btree (b) INCLUDE (a, a)"],
            ["m_named", "2", "1", "1 2", "true", "false", "CREATE UNIQUE INDEX m_named ON public.m USING btree (a) INCLUDE (b)"],
            ["u_a_b_key", "2", "1", "1 2", "true", "false", "CREATE UNIQUE INDEX u_a_b_key ON public.u USING btree (a) INCLUDE (b)"],
            ["x_a_b_c_key", "3", "2", "1 2 3", "true", "false", "CREATE UNIQUE INDEX x_a_b_c_key ON public.x USING btree (a, b) INCLUDE (c)"],
            ["x_pkey", "2", "1", "3 1", "true", "true", "CREATE UNIQUE INDEX x_pkey ON public.x USING btree (c) INCLUDE (a)"],
        ]
    );
    assert_eq!(
        rows(
            engine,
            "SELECT attrelid::regclass::text,attnum::text,attname::text FROM pg_attribute WHERE attrelid IN('x_a_b_c_key'::regclass,'x_pkey'::regclass,'m_b_a_a1_key'::regclass) ORDER BY 1,2"
        ),
        [
            ["m_b_a_a1_key", "1", "b"],
            ["m_b_a_a1_key", "2", "a"],
            ["m_b_a_a1_key", "3", "a1"],
            ["x_a_b_c_key", "1", "a"],
            ["x_a_b_c_key", "2", "b"],
            ["x_a_b_c_key", "3", "c"],
            ["x_pkey", "1", "c"],
            ["x_pkey", "2", "a"],
        ]
    );
}

#[test]
fn key_constraints_carry_included_columns_with_postgresql_names_and_catalog_rows() {
    let engine = Engine::new();
    create(&engine);
    catalog_rows_match_postgresql(&engine);
    // Only the key is unique, and only a primary key's key columns become NOT NULL.
    exec(
        &engine,
        "INSERT INTO u VALUES(1,1,1),(2,1,1); INSERT INTO x VALUES(NULL,NULL,1),(NULL,NULL,2)",
    );
    error(
        &engine,
        "INSERT INTO u VALUES(1,2,2)",
        "23505",
        "duplicate key value violates unique constraint \"u_a_b_key\"",
    );
    assert_eq!(rows(&engine, "SELECT b FROM u WHERE a = 2"), [["1"]]);
}

#[test]
fn included_columns_of_key_constraints_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("included.db");
    {
        let engine = Engine::open(&path).unwrap();
        create(&engine);
        exec(&engine, "INSERT INTO u VALUES(1,10,100),(2,20,200)");
    }
    let engine = Engine::open(&path).unwrap();
    catalog_rows_match_postgresql(&engine);
    assert_eq!(rows(&engine, "SELECT b FROM u WHERE a = 2"), [["20"]]);
    error(
        &engine,
        "INSERT INTO u VALUES(2,21,201)",
        "23505",
        "duplicate key value violates unique constraint \"u_a_b_key\"",
    );
}

#[test]
fn included_columns_of_key_constraints_must_exist_and_be_stored() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE m(a int,b int)");
    for sql in [
        "CREATE TABLE bad(a int,b int,UNIQUE(a) INCLUDE(nope))",
        "ALTER TABLE m ADD UNIQUE(a) INCLUDE(nope)",
        "ALTER TABLE m ADD PRIMARY KEY(a) INCLUDE(nope)",
    ] {
        error(
            &engine,
            sql,
            "42703",
            "column \"nope\" named in key does not exist",
        );
    }
    for (sql, message) in [
        (
            "CREATE TABLE g(a int,v int GENERATED ALWAYS AS (a + 1) VIRTUAL,UNIQUE(a) INCLUDE(v))",
            "unique constraints on virtual generated columns are not supported",
        ),
        (
            "CREATE TABLE g(a int,v int GENERATED ALWAYS AS (a + 1) VIRTUAL,PRIMARY KEY(a) INCLUDE(v))",
            "primary keys on virtual generated columns are not supported",
        ),
        (
            "CREATE TABLE g(a int,v int GENERATED ALWAYS AS (a + 1) VIRTUAL,UNIQUE(v))",
            "unique constraints on virtual generated columns are not supported",
        ),
        (
            "CREATE TABLE g(a int,v int GENERATED ALWAYS AS (a + 1) VIRTUAL,PRIMARY KEY(v))",
            "primary keys on virtual generated columns are not supported",
        ),
    ] {
        error(&engine, sql, "0A000", message);
    }
    assert!(engine
        .sql("CREATE TABLE col(a int UNIQUE INCLUDE(b),b int)", &[])
        .is_err());
}

#[test]
fn renaming_and_dropping_an_included_column_follow_the_constraint() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE u(a int,b int,c int,d int,CONSTRAINT u_key UNIQUE(a) INCLUDE(b,c)); INSERT INTO u VALUES(1,2,3,4)",
    );
    exec(&engine, "ALTER TABLE u RENAME COLUMN b TO bb");
    assert_eq!(
        rows(&engine, "SELECT pg_get_indexdef('u_key'::regclass)"),
        [["CREATE UNIQUE INDEX u_key ON public.u USING btree (a) INCLUDE (bb, c)"]]
    );
    assert_eq!(
        rows(&engine, "SELECT bb, c FROM u WHERE a = 1"),
        [["2", "3"]]
    );
    // The constraint goes with a column its index includes.
    exec(&engine, "ALTER TABLE u DROP COLUMN c");
    assert_eq!(
        rows(
            &engine,
            "SELECT count(*)::text FROM pg_constraint WHERE conrelid='u'::regclass AND contype='u'"
        ),
        [["0"]]
    );
    assert_eq!(
        rows(&engine, "SELECT to_regclass('u_key')::text"),
        [["NULL"]]
    );
    exec(&engine, "INSERT INTO u VALUES(1,5,6)");

    exec(
        &engine,
        "CREATE TABLE pp(a int,b int,c int,PRIMARY KEY(a) INCLUDE(b)); ALTER TABLE pp DROP COLUMN b",
    );
    assert_eq!(
        rows(
            &engine,
            "SELECT contype::text FROM pg_constraint WHERE conrelid='pp'::regclass ORDER BY 1"
        ),
        [["n"]]
    );
    assert_eq!(
        rows(
            &engine,
            "SELECT attname::text,attnotnull::text FROM pg_attribute WHERE attrelid='pp'::regclass AND attnum>0 AND NOT attisdropped ORDER BY attnum"
        ),
        [["a", "true"], ["c", "false"]]
    );
}

#[test]
fn a_foreign_key_on_the_constraint_depends_on_its_included_column() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE fk_parent(a int,b int,UNIQUE(a) INCLUDE(b)); CREATE TABLE fk_child(x int REFERENCES fk_parent(a))",
    );
    let restricted = engine
        .sql("ALTER TABLE fk_parent DROP COLUMN b", &[])
        .unwrap_err();
    assert_eq!(restricted.sqlstate(), Some("2BP01"), "{restricted}");
    assert_eq!(
        rows(
            &engine,
            "SELECT conname::text FROM pg_constraint WHERE conrelid IN('fk_parent'::regclass,'fk_child'::regclass) ORDER BY conname"
        ),
        [["fk_child_x_fkey"], ["fk_parent_a_b_key"]]
    );
    exec(&engine, "ALTER TABLE fk_parent DROP COLUMN b CASCADE");
    assert_eq!(
        rows(
            &engine,
            "SELECT count(*)::text FROM pg_constraint WHERE conrelid IN('fk_parent'::regclass,'fk_child'::regclass)"
        ),
        [["0"]]
    );
    exec(
        &engine,
        "INSERT INTO fk_child VALUES(7); INSERT INTO fk_parent VALUES(1),(1)",
    );
}
