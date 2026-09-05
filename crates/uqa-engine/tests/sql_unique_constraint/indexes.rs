//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone unique-index enforcement and catalog persistence.

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

fn count(engine: &Engine, table: &str) -> i64 {
    let result = engine
        .sql(&format!("SELECT count(*) AS n FROM {table}"), &[])
        .unwrap();
    let Value::Int(count) = result.rows[0]["n"] else {
        panic!("invalid count")
    };
    count
}

#[test]
fn standalone_unique_index_rejects_duplicates_and_rolls_back_whole_statements() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int,b text); CREATE UNIQUE INDEX items_ab ON items(a,b)",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(1,'one'),(NULL,'one'),(NULL,'one'),(1,NULL),(1,NULL)",
    );
    error(
        &engine,
        "INSERT INTO items VALUES(2,'two'),(1,'one')",
        "23505",
    );
    assert_eq!(count(&engine, "items"), 5);
    error(
        &engine,
        "UPDATE items SET a=1,b='one' WHERE a IS NULL",
        "23505",
    );
    assert_eq!(count(&engine, "items WHERE a IS NULL"), 2);
    exec(
        &engine,
        "DELETE FROM items WHERE a=1 AND b='one'; INSERT INTO items VALUES(1,'one')",
    );
}

#[test]
fn nulls_not_distinct_uses_every_matching_conflict_arbiter() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,b text); CREATE UNIQUE INDEX distinct_key ON items(a); CREATE UNIQUE INDEX exact_key ON items(a) NULLS NOT DISTINCT");
    exec(&engine, "INSERT INTO items VALUES(NULL,'first')");
    error(
        &engine,
        "INSERT INTO items VALUES(NULL,'duplicate')",
        "23505",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(NULL,'updated') ON CONFLICT(a) DO UPDATE SET b=excluded.b",
    );
    assert_eq!(count(&engine, "items"), 1);
    assert_eq!(
        engine.sql("SELECT b FROM items", &[]).unwrap().rows[0]["b"],
        Value::Str("updated".into())
    );
}

#[test]
fn unique_index_build_failure_does_not_publish_catalog_state() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int); INSERT INTO items VALUES(1),(1)",
    );
    error(
        &engine,
        "CREATE UNIQUE INDEX candidate ON items(a)",
        "23505",
    );
    assert!(engine
        .sql("SELECT * FROM pg_indexes WHERE indexname='candidate'", &[])
        .unwrap()
        .rows
        .is_empty());
    exec(&engine, "CREATE INDEX candidate ON items(a)");
    exec(
        &engine,
        "CREATE UNIQUE INDEX IF NOT EXISTS candidate ON items(a)",
    );
    exec(&engine, "INSERT INTO items VALUES(1)");
    assert_eq!(count(&engine, "items"), 3);
}

#[test]
fn partial_unique_keys_check_both_existing_and_proposed_predicates() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,active boolean); INSERT INTO items VALUES(1,false),(1,false),(1,NULL); CREATE UNIQUE INDEX active_key ON items(a) WHERE active");
    exec(&engine, "INSERT INTO items VALUES(1,true)");
    error(&engine, "INSERT INTO items VALUES(1,true)", "23505");
    error(
        &engine,
        "UPDATE items SET active=true WHERE active IS NULL",
        "23505",
    );
    exec(
        &engine,
        "UPDATE items SET active=false WHERE active; INSERT INTO items VALUES(1,true)",
    );
    exec(&engine, "INSERT INTO items VALUES(1,false),(1,false) ON CONFLICT(a) WHERE active DO UPDATE SET active=false");
    assert_eq!(count(&engine, "items"), 7);
}

#[test]
fn partial_index_inference_requires_an_implied_predicate() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,b int); CREATE UNIQUE INDEX positive_key ON items(a) WHERE b>0; INSERT INTO items VALUES(1,2)");
    error(
        &engine,
        "INSERT INTO items VALUES(1,3) ON CONFLICT(a) DO NOTHING",
        "42P10",
    );
    error(
        &engine,
        "INSERT INTO items VALUES(1,3) ON CONFLICT(a) WHERE b>=0 DO NOTHING",
        "42P10",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(1,3) ON CONFLICT(a) WHERE b>1 DO UPDATE SET b=excluded.b",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(1,3) ON CONFLICT(a) WHERE b>0 AND a>0 DO NOTHING",
    );
    assert_eq!(count(&engine, "items"), 1);
}

#[test]
fn partial_index_conflict_overlay_keeps_command_cardinality() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int,b int); CREATE UNIQUE INDEX positive_key ON items(a) WHERE b>0",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(1,-1),(1,1),(1,1) ON CONFLICT DO NOTHING",
    );
    assert_eq!(count(&engine, "items"), 2);
    error(
        &engine,
        "INSERT INTO items VALUES(2,1),(2,2) ON CONFLICT(a) WHERE b>0 DO UPDATE SET b=excluded.b",
        "21000",
    );
    assert_eq!(count(&engine, "items"), 2);
}

#[test]
fn unique_index_drop_and_transaction_rollback_release_enforcement() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int); INSERT INTO items VALUES(1)",
    );
    exec(
        &engine,
        "BEGIN; CREATE UNIQUE INDEX candidate ON items(a); ROLLBACK",
    );
    exec(&engine, "INSERT INTO items VALUES(1)");
    exec(&engine, "TRUNCATE items; CREATE UNIQUE INDEX candidate ON items(a); INSERT INTO items VALUES(1); DROP INDEX candidate; INSERT INTO items VALUES(1)");
    assert_eq!(count(&engine, "items"), 2);
}

#[test]
fn named_constraint_arbitration_does_not_use_unrelated_unique_indexes() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int CONSTRAINT a_key UNIQUE,b int UNIQUE); INSERT INTO items VALUES(1,1)");
    error(
        &engine,
        "INSERT INTO items VALUES(2,1) ON CONFLICT ON CONSTRAINT a_key DO NOTHING",
        "23505",
    );
    exec(
        &engine,
        "INSERT INTO items VALUES(1,2) ON CONFLICT ON CONSTRAINT a_key DO NOTHING",
    );
    error(
        &engine,
        "INSERT INTO items VALUES(1,1) ON CONFLICT ON CONSTRAINT missing DO NOTHING",
        "42704",
    );
    assert_eq!(count(&engine, "items"), 1);
}

#[test]
fn renamed_partial_index_predicate_and_null_semantics_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("indexes.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE items(a int,active boolean); CREATE UNIQUE INDEX active_key ON items(a) NULLS NOT DISTINCT WHERE active; INSERT INTO items VALUES(NULL,true),(NULL,false)");
        exec(&engine, "ALTER TABLE items RENAME COLUMN active TO enabled; ALTER TABLE items RENAME COLUMN a TO k; ALTER TABLE items RENAME TO records");
    }
    let engine = Engine::open(&database).unwrap();
    error(&engine, "INSERT INTO records VALUES(NULL,true)", "23505");
    exec(&engine, "INSERT INTO records VALUES(NULL,false)");
    error(
        &engine,
        "UPDATE records SET enabled=true WHERE NOT enabled",
        "23505",
    );
    assert_eq!(count(&engine, "records"), 3);
}

#[test]
fn index_predicates_bind_immutable_functions_and_declared_types() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,b text)");
    error(
        &engine,
        "CREATE UNIQUE INDEX bad ON items(a) WHERE random()>0",
        "42P17",
    );
    error(
        &engine,
        "CREATE UNIQUE INDEX bad ON items(a) WHERE (SELECT true)",
        "0A000",
    );
    error(
        &engine,
        "CREATE UNIQUE INDEX bad ON items(a) WHERE a",
        "42804",
    );
    exec(&engine, "CREATE UNIQUE INDEX lower_key ON items(a) WHERE lower(b)='yes'; INSERT INTO items VALUES(1,'YES'),(1,'no')");
    error(&engine, "INSERT INTO items VALUES(1,'yes')", "23505");
}

#[test]
fn unique_index_foreign_key_dependency_survives_reopen_and_cascades() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("references.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        exec(&engine, "CREATE TABLE parent(a int); CREATE UNIQUE INDEX parent_key ON parent(a); CREATE TABLE child(a int REFERENCES parent(a))");
        exec(
            &engine,
            "INSERT INTO parent VALUES(1); INSERT INTO child VALUES(1)",
        );
    }
    let engine = Engine::open(&database).unwrap();
    error(&engine, "INSERT INTO child VALUES(2)", "23503");
    error(&engine, "DROP INDEX parent_key", "2BP01");
    exec(
        &engine,
        "DROP INDEX parent_key CASCADE; INSERT INTO child VALUES(2); INSERT INTO parent VALUES(1)",
    );
    assert_eq!(count(&engine, "child"), 2);
    assert_eq!(count(&engine, "parent"), 2);
}

#[test]
fn predicate_function_identity_survives_rename_and_owns_drop_dependency() {
    let engine = Engine::new();
    exec(&engine, "CREATE FUNCTION positive(int) RETURNS boolean LANGUAGE SQL IMMUTABLE RETURN $1>0; CREATE TABLE items(a int,b int); CREATE UNIQUE INDEX positive_key ON items(a) WHERE positive(b); INSERT INTO items VALUES(1,1)");
    error(&engine, "DROP FUNCTION positive(int)", "2BP01");
    exec(
        &engine,
        "ALTER FUNCTION positive(int) RENAME TO renamed_positive",
    );
    error(&engine, "INSERT INTO items VALUES(1,2)", "23505");
    exec(
        &engine,
        "DROP FUNCTION renamed_positive(int) CASCADE; INSERT INTO items VALUES(1,2)",
    );
    assert_eq!(count(&engine, "items"), 2);
}

#[test]
fn index_definitions_and_constraint_indexes_report_real_semantics() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int PRIMARY KEY,b text UNIQUE,c int); CREATE UNIQUE INDEX covering ON items(a DESC NULLS LAST,c) INCLUDE(b) NULLS NOT DISTINCT WHERE c>0");
    let result = engine.sql("SELECT pg_get_indexdef('covering'::regclass) AS definition,pg_get_indexdef('covering'::regclass,3,false) AS included,pg_get_indexdef('covering'::regclass,-1,false) AS invalid", &[]).unwrap();
    assert_eq!(result.rows[0]["definition"], Value::Str("CREATE UNIQUE INDEX covering ON public.items USING btree (a DESC NULLS LAST, c) INCLUDE (b) NULLS NOT DISTINCT WHERE (c > 0)".into()));
    assert_eq!(result.rows[0]["included"], Value::Str("b".into()));
    assert_eq!(result.rows[0]["invalid"], Value::Str(String::new()));
    let primary = engine
        .sql(
            "SELECT indisunique,indisprimary FROM pg_index WHERE indexrelid='items_pkey'::regclass",
            &[],
        )
        .unwrap();
    assert_eq!(primary.rows[0]["indisunique"], Value::Bool(true));
    assert_eq!(primary.rows[0]["indisprimary"], Value::Bool(true));
    error(&engine, "DROP INDEX items_pkey CASCADE", "2BP01");
    exec(&engine, "ALTER TABLE items DROP CONSTRAINT items_pkey");
    assert_eq!(
        engine
            .sql("SELECT to_regclass('items_pkey') AS id", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Null
    );
}

#[test]
fn partial_unique_indexes_preserve_metadata_in_key_value_backends() {
    for redb in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("indexes.db");
        let open = || {
            let provider: std::sync::Arc<dyn uqa_storage::PersistentStorageProvider> = if redb {
                std::sync::Arc::new(uqa_storage_redb::RedbStorage::open(&database).unwrap())
            } else {
                std::sync::Arc::new(
                    uqa_storage_sqlite::SQLiteKeyValueStorage::open(&database).unwrap(),
                )
            };
            Engine::from_persistent_provider(provider).unwrap()
        };
        {
            let engine = open();
            exec(&engine, "CREATE TABLE items(a int,b int); CREATE UNIQUE INDEX item_key ON items(a) NULLS NOT DISTINCT WHERE b>0; INSERT INTO items VALUES(NULL,1),(NULL,-1)");
            exec(&engine, "ALTER TABLE items RENAME COLUMN b TO score");
        }
        let engine = open();
        error(&engine, "INSERT INTO items VALUES(NULL,2)", "23505");
        exec(&engine, "INSERT INTO items VALUES(NULL,-2)");
        assert_eq!(count(&engine, "items"), 3);
        let definition = engine
            .sql(
                "SELECT pg_get_indexdef('item_key'::regclass) AS definition",
                &[],
            )
            .unwrap();
        assert_eq!(definition.rows[0]["definition"], Value::Str("CREATE UNIQUE INDEX item_key ON public.items USING btree (a) NULLS NOT DISTINCT WHERE (score > 0)".into()));
    }
}

#[test]
fn conflict_target_is_validated_even_without_input_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int,b int); CREATE UNIQUE INDEX positive_key ON items(a) WHERE b>0",
    );
    error(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT(a) DO NOTHING",
        "42P10",
    );
    exec(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT(a) WHERE b>0 DO NOTHING",
    );
}

#[test]
fn constraint_index_names_respect_relation_collisions() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE source(a int); CREATE INDEX candidate_pkey ON source(a); CREATE TABLE candidate(a int PRIMARY KEY)");
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_indexdef('candidate_pkey1'::regclass) AS definition",
                &[]
            )
            .unwrap()
            .rows[0]["definition"],
        Value::Str(
            "CREATE UNIQUE INDEX candidate_pkey1 ON public.candidate USING btree (a)".into()
        )
    );
    error(
        &engine,
        "CREATE TABLE conflicting(a int CONSTRAINT candidate_pkey1 UNIQUE)",
        "42P07",
    );
    error(
        &engine,
        "ALTER TABLE source ADD CONSTRAINT candidate_pkey1 UNIQUE(a)",
        "42P07",
    );
    assert!(
        engine
            .sql("SELECT to_regclass('conflicting') AS id", &[])
            .unwrap()
            .rows[0]["id"]
            == Value::Null
    );
}

#[test]
fn partition_constraint_indexes_have_one_child_identity() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE partitioned(a int PRIMARY KEY) PARTITION BY RANGE(a); CREATE TABLE low PARTITION OF partitioned FOR VALUES FROM (0) TO (10)");
    let result = engine.sql("SELECT c.relname,c.relispartition,i.indisprimary FROM pg_class c JOIN pg_index i ON i.indexrelid=c.oid WHERE i.indrelid IN ('partitioned'::regclass,'low'::regclass) ORDER BY c.relname", &[]).unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["relname"], Value::Str("low_pkey".into()));
    assert_eq!(result.rows[0]["relispartition"], Value::Bool(true));
    assert_eq!(
        result.rows[1]["relname"],
        Value::Str("partitioned_pkey".into())
    );
    exec(&engine, "INSERT INTO partitioned VALUES(1)");
    error(&engine, "INSERT INTO low VALUES(1)", "23505");
}

#[test]
fn copy_uses_partial_unique_keys_and_rolls_back_duplicates() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,b boolean); CREATE UNIQUE INDEX active_key ON items(a) NULLS NOT DISTINCT WHERE b");
    engine
        .copy_from("COPY items FROM STDIN", b"1\tf\n1\tf\n1\tt\n".as_slice())
        .unwrap();
    let failure = engine
        .copy_from("COPY items FROM STDIN", b"2\tt\n1\tt\n".as_slice())
        .unwrap_err();
    assert_eq!(failure.sqlstate(), Some("23505"));
    assert_eq!(count(&engine, "items"), 3);
}

#[test]
fn inference_predicates_follow_view_columns_and_target_aliases() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE items(a int,b int,c text); CREATE UNIQUE INDEX active_key ON items(a) WHERE b>0; CREATE VIEW exposed AS SELECT a AS key,b AS amount,c AS label,b>0 AS active FROM items");
    exec(&engine, "INSERT INTO exposed(key,amount,label) VALUES(1,2,'first') ON CONFLICT(key) WHERE active DO NOTHING");
    exec(&engine, "INSERT INTO exposed AS v(key,amount,label) VALUES(1,3,'updated') ON CONFLICT(key) WHERE v.active DO UPDATE SET label=excluded.label");
    let result = engine.sql("SELECT * FROM items", &[]).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["b"], Value::Int(2));
    assert_eq!(result.rows[0]["c"], Value::Str("updated".into()));
    exec(&engine, "INSERT INTO items AS x VALUES(1,4,'unused') ON CONFLICT(a) WHERE x.b>0 AND random()>0 DO NOTHING");
    error(&engine, "INSERT INTO exposed(key,amount,label) SELECT 1,2,'x' WHERE false ON CONFLICT(key) WHERE missing DO NOTHING", "42703");
}

#[test]
fn inference_analysis_distinguishes_unknown_columns_and_non_index_constraints() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items(a int CONSTRAINT positive CHECK(a>0), b int UNIQUE)",
    );
    error(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT ON CONSTRAINT positive DO NOTHING",
        "42809",
    );
    error(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT ON CONSTRAINT missing DO NOTHING",
        "42704",
    );
    error(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT(missing) DO NOTHING",
        "42703",
    );
    exec(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT(b) WHERE a DO NOTHING",
    );
    error(
        &engine,
        "INSERT INTO items SELECT 1,1 WHERE false ON CONFLICT(b) WHERE (SELECT true) DO NOTHING",
        "0A000",
    );
}
