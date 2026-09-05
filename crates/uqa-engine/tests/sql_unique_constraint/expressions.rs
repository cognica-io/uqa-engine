//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression-index builds, mutations, inference, and durable object bindings.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn error(engine: &Engine, sql: &str, state: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
}

#[test]
fn unique_expression_keys_enforce_build_and_mutation_atomicity() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE accounts(email text,tenant int); INSERT INTO accounts VALUES('One',1),('one',1)");
    error(
        &engine,
        "CREATE UNIQUE INDEX folded_email ON accounts(lower(email),tenant)",
        "23505",
    );
    assert_eq!(
        engine
            .sql("SELECT to_regclass('folded_email') AS oid", &[])
            .unwrap()
            .rows[0]["oid"],
        Value::Null
    );
    exec(&engine, "DELETE FROM accounts WHERE email='one'; CREATE UNIQUE INDEX folded_email ON accounts(lower(email),tenant) NULLS NOT DISTINCT");
    error(
        &engine,
        "INSERT INTO accounts VALUES('Two',1),('ONE',1)",
        "23505",
    );
    exec(&engine, "INSERT INTO accounts VALUES('Two',1),(NULL,1)");
    error(
        &engine,
        "UPDATE accounts SET email='ONE' WHERE email='Two'",
        "23505",
    );
    error(&engine, "INSERT INTO accounts VALUES(NULL,1)", "23505");
    exec(
        &engine,
        "DELETE FROM accounts WHERE email='One'; INSERT INTO accounts VALUES('one',1)",
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM accounts", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(3)
    );
}

#[test]
fn expression_arbiters_bind_aliases_and_partial_view_predicates() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE accounts(email text,tenant int,active boolean,n int); CREATE UNIQUE INDEX folded_active ON accounts(lower(email),tenant) WHERE active; CREATE VIEW exposed AS SELECT email AS address,tenant AS customer,active AS enabled,n AS value FROM accounts");
    exec(&engine, "INSERT INTO exposed VALUES('One',1,true,1) ON CONFLICT(customer,lower(address)) WHERE enabled DO NOTHING");
    exec(&engine, "INSERT INTO exposed AS v VALUES('one',1,true,2) ON CONFLICT(lower(v.address),customer) WHERE enabled DO UPDATE SET value=excluded.value");
    assert_eq!(
        engine.sql("SELECT n FROM accounts", &[]).unwrap().rows[0]["n"],
        Value::Int(2)
    );
    error(&engine, "INSERT INTO accounts VALUES('one',1,true,3) ON CONFLICT(upper(email),tenant) WHERE active DO NOTHING", "42P10");
    error(
        &engine,
        "INSERT INTO accounts VALUES('one',1,true,3) ON CONFLICT(lower(email),tenant) DO NOTHING",
        "42P10",
    );
    exec(&engine, "INSERT INTO accounts VALUES('one',1,false,3)");
    error(
        &engine,
        "UPDATE accounts SET active=true WHERE NOT active",
        "23505",
    );
}

#[test]
fn expression_keys_reopen_with_column_and_routine_identity() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("expression-index.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE FUNCTION folded(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN lower(x); CREATE TABLE accounts(email text); CREATE UNIQUE INDEX folded_email ON accounts(folded(email)); INSERT INTO accounts VALUES('One'); ALTER TABLE accounts RENAME COLUMN email TO address; ALTER FUNCTION folded(text) RENAME TO normalized");
    }
    let engine = Engine::open(&database).unwrap();
    error(&engine, "INSERT INTO accounts VALUES('one')", "23505");
    error(&engine, "DROP FUNCTION normalized(text)", "2BP01");
    let definition = engine
        .sql(
            "SELECT pg_get_indexdef('folded_email'::regclass) AS definition",
            &[],
        )
        .unwrap();
    assert_eq!(
        definition.rows[0]["definition"],
        Value::Str(
            "CREATE UNIQUE INDEX folded_email ON public.accounts USING btree (normalized(address))"
                .into()
        )
    );
    exec(
        &engine,
        "DROP FUNCTION normalized(text) CASCADE; INSERT INTO accounts VALUES('one')",
    );
}

fn open_expression_engine(path: &std::path::Path, backend: u8) -> Engine {
    let provider: std::sync::Arc<dyn uqa_storage::PersistentStorageProvider> = match backend {
        0 => return Engine::open(path).unwrap(),
        1 => std::sync::Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap()),
        2 => std::sync::Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()),
        _ => unreachable!("test backend"),
    };
    Engine::from_persistent_provider(provider).unwrap()
}

#[test]
fn physical_expression_keys_keep_namespace_and_old_values_in_every_backend() {
    for backend in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("keys.db");
        {
            let engine = open_expression_engine(&database, backend);
            exec(&engine, "CREATE FUNCTION key_value(x int) RETURNS int IMMUTABLE LANGUAGE SQL RETURN x; CREATE TABLE items(n int, \"public.expr_key\" int UNIQUE); CREATE UNIQUE INDEX expr_key ON items(key_value(n)); INSERT INTO items VALUES(1,90); CREATE OR REPLACE FUNCTION key_value(x int) RETURNS int IMMUTABLE LANGUAGE SQL RETURN x+10");
        }
        {
            let engine = open_expression_engine(&database, backend);
            // The existing key is still 1 after replacing its function. Re-evaluating the old row would incorrectly hide this conflict.
            error(&engine, "INSERT INTO items VALUES(-9,91)", "23505");
            exec(
                &engine,
                "UPDATE items SET n=2; INSERT INTO items VALUES(-9,91)",
            );
            error(&engine, "INSERT INTO items VALUES(7,91)", "23505");
            exec(
                &engine,
                "DELETE FROM items WHERE n=-9; INSERT INTO items VALUES(-9,92)",
            );
            exec(&engine, "BEGIN; SAVEPOINT before_change; UPDATE items SET n=3 WHERE n=2; ROLLBACK TO before_change; COMMIT");
            error(&engine, "INSERT INTO items VALUES(2,93)", "23505");
            exec(&engine, "INSERT INTO items VALUES(3,93)");
        }
        let engine = open_expression_engine(&database, backend);
        error(&engine, "INSERT INTO items VALUES(2,94)", "23505");
        assert_eq!(
            engine
                .sql(
                    "SELECT count(*) AS n FROM items WHERE \"public.expr_key\">=90",
                    &[]
                )
                .unwrap()
                .rows[0]["n"],
            Value::Int(3)
        );
        exec(
            &engine,
            "TRUNCATE items; INSERT INTO items VALUES(2,94); ALTER TABLE items DROP COLUMN n",
        );
        assert_eq!(
            engine
                .sql("SELECT to_regclass('expr_key') AS i", &[])
                .unwrap()
                .rows[0]["i"],
            Value::Null
        );
        drop(engine);
        let engine = open_expression_engine(&database, backend);
        assert_eq!(
            engine
                .sql("SELECT to_regclass('expr_key') AS i", &[])
                .unwrap()
                .rows[0]["i"],
            Value::Null
        );
        error(&engine, "INSERT INTO items VALUES(94)", "23505");
    }
}

#[test]
fn partial_expression_indexes_copy_and_rollback_in_every_backend() {
    for backend in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("partial.db");
        {
            let engine = open_expression_engine(&database, backend);
            exec(&engine, "CREATE TABLE items(email text,active boolean); CREATE UNIQUE INDEX active_email ON items(lower(email)) NULLS NOT DISTINCT WHERE active");
            engine
                .copy_from(
                    "COPY items FROM STDIN",
                    b"One\tt\none\tf\n\\N\tt\n".as_slice(),
                )
                .unwrap();
            let failure = engine
                .copy_from("COPY items FROM STDIN", b"Two\tt\nONE\tt\n".as_slice())
                .unwrap_err();
            assert_eq!(failure.sqlstate(), Some("23505"));
            exec(&engine, "BEGIN; DELETE FROM items WHERE active; INSERT INTO items VALUES('one',true); ROLLBACK");
            error(
                &engine,
                "UPDATE items SET active=true WHERE NOT active",
                "23505",
            );
            exec(&engine, "ALTER TABLE items RENAME TO accounts; ALTER TABLE accounts RENAME COLUMN email TO address; ALTER TABLE accounts RENAME COLUMN active TO enabled");
        }
        let engine = open_expression_engine(&database, backend);
        error(&engine, "INSERT INTO accounts VALUES('ONE',true)", "23505");
        error(&engine, "INSERT INTO accounts VALUES(NULL,true)", "23505");
        exec(
            &engine,
            "INSERT INTO accounts VALUES('Two',true),('ONE',NULL)",
        );
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM accounts", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(5)
        );
    }
}

#[test]
fn expression_keys_cover_existing_new_and_attached_partitions() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(tenant int,email text) PARTITION BY RANGE(tenant); CREATE TABLE low PARTITION OF items FOR VALUES FROM(0) TO(10); INSERT INTO low VALUES(1,'One'); CREATE UNIQUE INDEX tenant_email ON items(tenant,lower(email)); CREATE TABLE high PARTITION OF items FOR VALUES FROM(10) TO(20)");
    error(&engine, "INSERT INTO low VALUES(1,'one')", "23505");
    exec(&engine, "INSERT INTO items VALUES(11,'One')");
    error(&engine, "INSERT INTO high VALUES(11,'ONE')", "23505");
    exec(&engine, "CREATE TABLE candidate(tenant int,email text); INSERT INTO candidate VALUES(21,'One'),(21,'one')");
    error(
        &engine,
        "ALTER TABLE items ATTACH PARTITION candidate FOR VALUES FROM(20) TO(30)",
        "23505",
    );
    exec(&engine, "DELETE FROM candidate WHERE email='one'; ALTER TABLE items ATTACH PARTITION candidate FOR VALUES FROM(20) TO(30)");
    error(&engine, "INSERT INTO candidate VALUES(21,'ONE')", "23505");
}

#[test]
fn expression_runtime_errors_keep_sqlstate_and_build_atomicity() {
    for backend in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("expression-errors.db");
        let engine = if backend == 3 {
            Engine::new()
        } else {
            open_expression_engine(&database, backend)
        };
        exec(
            &engine,
            "CREATE TABLE ratios(n int); INSERT INTO ratios VALUES(0)",
        );
        error(&engine, "CREATE INDEX ratio_key ON ratios((10/n))", "22012");
        assert_eq!(
            engine
                .sql("SELECT to_regclass('ratio_key') AS oid", &[])
                .unwrap()
                .rows[0]["oid"],
            Value::Null
        );
        exec(
            &engine,
            "UPDATE ratios SET n=1; CREATE INDEX ratio_key ON ratios((10/n))",
        );
        let failure = engine
            .sql("INSERT INTO ratios VALUES(2),(0)", &[])
            .expect_err(&format!(
                "backend {backend} accepted an invalid expression key"
            ));
        assert_eq!(
            failure.sqlstate(),
            Some("22012"),
            "backend {backend}: {failure}"
        );
        assert_eq!(
            engine.sql("SELECT n FROM ratios", &[]).unwrap().rows[0]["n"],
            Value::Int(1)
        );
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM ratios", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(1)
        );
        exec(&engine, "VACUUM FULL ratios");
        error(&engine, "UPDATE ratios SET n=0", "22012");
    }
}

#[test]
fn temporary_expression_keys_preserve_stored_values_across_rollback() {
    for backend in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("temporary-expression-keys.db");
        let engine = if backend == 3 {
            Engine::new()
        } else {
            open_expression_engine(&database, backend)
        };
        exec(&engine, "CREATE FUNCTION transform_key(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN lower(x); CREATE TEMP TABLE labels(label text); CREATE UNIQUE INDEX labels_key ON labels(transform_key(label)); INSERT INTO labels VALUES('One')");
        exec(&engine, "CREATE OR REPLACE FUNCTION transform_key(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN upper(x); INSERT INTO labels VALUES('one')");
        exec(&engine, "BEGIN; SAVEPOINT before_delete; DELETE FROM labels WHERE label='One'; ROLLBACK TO before_delete; COMMIT");
        error(&engine, "INSERT INTO labels VALUES('ONE')", "23505");
        exec(&engine, "CREATE OR REPLACE FUNCTION transform_key(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN lower(x)");
        error(&engine, "INSERT INTO labels VALUES('ONE')", "23505");
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM labels", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(2)
        );
    }
}
