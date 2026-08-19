//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-level transaction lifecycle convenience methods for begin, commit,
//! rollback, and savepoint operations.

use uqa_engine::Engine;

#[test]
fn begin_commit_round_trip() {
    let eng = Engine::new();
    assert_eq!(eng.transaction_depth(), 0);
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn nested_begin_commit_pops_one_frame_at_a_time() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn savepoint_release_round_trip() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.release_savepoint("sp1").unwrap();
    eng.commit().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn rollback_to_savepoint_keeps_frame_open() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("sp1").unwrap();
    eng.rollback_to_savepoint("sp1").unwrap();
    assert_eq!(eng.transaction_depth(), 1);
    eng.rollback().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn savepoint_order_invalidates_descendants_and_preserves_shadowed_names() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.savepoint("dup").unwrap();
    eng.sql("CREATE SCHEMA first_level", &[]).unwrap();
    eng.savepoint("dup").unwrap();
    eng.sql("CREATE SCHEMA second_level", &[]).unwrap();

    eng.rollback_to_savepoint("dup").unwrap();
    assert!(eng.has_schema("first_level").unwrap());
    assert!(!eng.has_schema("second_level").unwrap());
    eng.release_savepoint("dup").unwrap();
    eng.rollback_to_savepoint("dup").unwrap();
    assert!(!eng.has_schema("first_level").unwrap());

    eng.savepoint("outer").unwrap();
    eng.savepoint("inner").unwrap();
    eng.rollback_to_savepoint("outer").unwrap();
    assert!(eng.release_savepoint("inner").is_err());
    // PostgreSQL 18: a failed savepoint command aborts the transaction, so
    // every later command except ROLLBACK reports 25P02 and COMMIT rolls back.
    let error = eng.release_savepoint("outer").unwrap_err();
    assert_eq!(error.sqlstate(), Some("25P02"));
    eng.rollback_to_savepoint("outer").unwrap();
    eng.release_savepoint("outer").unwrap();
    eng.commit().unwrap();
    assert!(!eng.has_schema("first_level").unwrap());
}

#[test]
fn close_drops_open_transactions() {
    let eng = Engine::new();
    eng.begin().unwrap();
    eng.begin().unwrap();
    assert_eq!(eng.transaction_depth(), 2);
    eng.close().unwrap();
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn commit_without_begin_errors() {
    let eng = Engine::new();
    assert!(eng.commit().is_err());
}

#[test]
fn side_effecting_selects_use_statement_rollback_in_memory() {
    let eng = Engine::new();

    // The inner graph function must run before the selected CASE branch
    // divides by zero. The failed SELECT must still remove the graph it created.
    let graph_error = eng
        .sql(
            "SELECT CASE WHEN graph_create('transient_graph')
                    THEN 1 / 0 ELSE 0 END",
            &[],
        )
        .unwrap_err();
    assert!(!graph_error.to_string().is_empty());
    assert!(!eng.has_graph("transient_graph").unwrap());

    // Mutating table functions live in SourcePlan rather than ScalarExpr.
    // Their source row feeds the failing cast, proving creation happened
    // before the outer projection failed and was rolled back.
    let analyzer_error = eng
        .sql(
            "SELECT CAST(created AS INTEGER)
             FROM create_analyzer(
                'transient_analyzer',
                '{\"tokenizer\":\"keyword\"}'
             ) AS made(created)",
            &[],
        )
        .unwrap_err();
    assert!(analyzer_error
        .to_string()
        .to_ascii_lowercase()
        .contains("integer"));
    assert!(!eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"transient_analyzer".to_string()));

    // Scalar mutators nested under both a subquery source and a CTE must be
    // discovered by the same classifier.
    eng.sql("CREATE TABLE udf_rows (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let nested_error = eng
        .sql(
            "WITH nested AS (
                SELECT changed
                FROM (SELECT graph_create('nested_graph') AS changed) AS child
             )
             SELECT CASE WHEN changed THEN 1 / 0 ELSE 0 END FROM nested",
            &[],
        )
        .unwrap_err();
    assert!(!nested_error.to_string().is_empty());
    assert!(!eng.has_graph("nested_graph").unwrap());

    // A registered routine can hide DML behind an ordinary Func node. The
    // outer type error happens after the INSERT and must roll it back.
    eng.sql(
        "CREATE FUNCTION mutate_then_return() RETURNS INTEGER AS $$
         BEGIN
           INSERT INTO udf_rows (id) VALUES (1);
           RETURN 1;
         END;
         $$ LANGUAGE plpgsql",
        &[],
    )
    .unwrap();
    eng.sql(
        "SELECT CASE WHEN mutate_then_return() = 1 THEN 1 / 0 ELSE 0 END",
        &[],
    )
    .unwrap_err();
    let count = eng.sql("SELECT count(*) AS n FROM udf_rows", &[]).unwrap();
    assert_eq!(count.rows[0]["n"], uqa_core::Value::Int(0));

    // cypher() can mutate an existing graph from a SourcePlan::Function.
    // Returning the created property makes the outer cast fail only after
    // the graph write has happened.
    eng.sql("SELECT create_graph('cypher_tx')", &[]).unwrap();
    eng.sql(
        "SELECT CAST(name AS INTEGER)
         FROM cypher('cypher_tx', $$
            CREATE (n:Person {name: 'not-an-integer'})
            RETURN n.name
         $$) AS (name text)",
        &[],
    )
    .unwrap_err();
    let cypher_rows = eng
        .sql(
            "SELECT * FROM cypher('cypher_tx', $$
                MATCH (n:Person) RETURN n.name
             $$) AS (name text)",
            &[],
        )
        .unwrap();
    assert!(cypher_rows.rows.is_empty());

    // random() mutates the per-session RNG. A failed outer expression must
    // restore the stream to the exact pre-statement position.
    let baseline = Engine::new();
    baseline.sql("SELECT setseed(0.25)", &[]).unwrap();
    let expected = baseline.sql("SELECT random() AS value", &[]).unwrap();
    eng.sql("SELECT setseed(0.25)", &[]).unwrap();
    eng.sql("SELECT CASE WHEN random() >= 0 THEN 1 / 0 ELSE 0 END", &[])
        .unwrap_err();
    let actual = eng.sql("SELECT random() AS value", &[]).unwrap();
    assert_eq!(actual.rows[0]["value"], expected.rows[0]["value"]);
}

#[test]
fn side_effecting_select_rollback_matches_memory_and_catalog_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("side_effecting_select.db");

    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "SELECT CASE WHEN graph_create('transient_graph')
                    THEN 1 / 0 ELSE 0 END",
            &[],
        )
        .unwrap_err();
        assert!(!eng.has_graph("transient_graph").unwrap());
    }

    let reopened = Engine::open(&path).unwrap();
    assert!(!reopened.has_graph("transient_graph").unwrap());
}

#[test]
fn explicit_memory_rollback_restores_every_sql_owned_registry() {
    let eng = Engine::new();
    eng.sql("SET work_mem = '8MB'", &[]).unwrap();
    eng.create_graph("base_graph").unwrap();

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("CREATE SCHEMA tx_schema", &[]).unwrap();
    eng.sql("SET search_path TO tx_schema, public", &[])
        .unwrap();
    eng.sql("SET work_mem = '16MB'", &[]).unwrap();
    eng.sql("CREATE VIEW tx_view AS SELECT 1 AS n", &[])
        .unwrap();
    eng.sql("CREATE SEQUENCE tx_sequence", &[]).unwrap();
    eng.sql("PREPARE tx_prepared AS SELECT 1 AS n", &[])
        .unwrap();
    eng.sql(
        "CREATE FUNCTION tx_function() RETURNS INTEGER
         LANGUAGE SQL AS 'SELECT 7'",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE SERVER tx_server FOREIGN DATA WRAPPER memory_fdw
         OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE tx_remote (id INTEGER)
         SERVER tx_server OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    eng.sql("SELECT graph_create('tx_graph')", &[]).unwrap();
    eng.sql(
        "SELECT * FROM create_analyzer(
            'tx_analyzer', '{\"tokenizer\":\"keyword\"}'
         )",
        &[],
    )
    .unwrap();
    eng.build_path_index("tx_path", "base_graph", &[vec!["knows".into()]])
        .unwrap();
    eng.save_scoring_params("tx_score", "{\"alpha\":1}")
        .unwrap();
    eng.save_model(
        "tx_model",
        &uqa_ml::DeepModel {
            layers: Vec::new(),
            alpha: 0.0,
            gating: uqa_ml::GatingSpec::None,
        },
    )
    .unwrap();
    eng.sql("ROLLBACK", &[]).unwrap();

    assert!(!eng.list_schemas().unwrap().contains(&"tx_schema".into()));
    assert!(!eng.list_views().unwrap().contains(&"tx_view".into()));
    assert!(!eng.has_graph("tx_graph").unwrap());
    assert!(!eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"tx_analyzer".into()));
    assert!(!eng
        .list_foreign_servers()
        .unwrap()
        .contains(&"tx_server".into()));
    assert!(!eng
        .list_foreign_tables()
        .unwrap()
        .contains(&"tx_remote".into()));
    assert!(eng.list_path_indexes().unwrap().is_empty());
    assert!(eng.load_scoring_params("tx_score").unwrap().is_none());
    assert!(eng.load_model("tx_model").unwrap().is_none());
    assert!(eng.sql("SELECT tx_function()", &[]).is_err());
    assert!(eng.sql("EXECUTE tx_prepared", &[]).is_err());
    assert!(eng.currval("tx_sequence").is_err());

    let work_mem = eng.sql("SHOW work_mem", &[]).unwrap();
    assert_eq!(
        work_mem.rows[0]["work_mem"],
        uqa_core::Value::Str("8MB".into())
    );
    assert_eq!(eng.search_path(), vec!["public"]);
}

#[test]
fn memory_savepoint_restores_registry_state_without_losing_earlier_changes() {
    let eng = Engine::new();
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("SELECT graph_create('before_savepoint')", &[])
        .unwrap();
    eng.sql("SAVEPOINT registry_point", &[]).unwrap();
    eng.sql("SELECT graph_create('after_savepoint')", &[])
        .unwrap();
    eng.sql("CREATE SCHEMA after_savepoint_schema", &[])
        .unwrap();

    eng.sql("ROLLBACK TO SAVEPOINT registry_point", &[])
        .unwrap();
    assert!(eng.has_graph("before_savepoint").unwrap());
    assert!(!eng.has_graph("after_savepoint").unwrap());
    assert!(!eng
        .list_schemas()
        .unwrap()
        .contains(&"after_savepoint_schema".into()));
    eng.sql("COMMIT", &[]).unwrap();
    assert!(eng.has_graph("before_savepoint").unwrap());
}

#[test]
fn persistent_rollback_and_savepoint_restore_lightweight_session_state() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("session-state.db")).unwrap();
    engine.sql("CREATE SCHEMA base", &[]).unwrap();
    engine.sql("SET search_path TO base, public", &[]).unwrap();
    engine.sql("SET work_mem = '8MB'", &[]).unwrap();
    engine.sql("SELECT setseed(0.25)", &[]).unwrap();

    let baseline = Engine::new();
    baseline.sql("SELECT setseed(0.25)", &[]).unwrap();
    let expected_outer = baseline.sql("SELECT random() AS value", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SET search_path TO public", &[]).unwrap();
    engine.sql("SET work_mem = '16MB'", &[]).unwrap();
    engine.sql("SELECT setseed(-0.5)", &[]).unwrap();
    engine
        .sql("PREPARE rolled_back AS SELECT 1 AS value", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();

    assert_eq!(engine.search_path(), vec!["base", "public"]);
    assert_eq!(
        engine.sql("SHOW work_mem", &[]).unwrap().rows[0]["work_mem"],
        uqa_core::Value::Str("8MB".into())
    );
    assert!(engine.sql("EXECUTE rolled_back", &[]).is_err());
    assert_eq!(
        engine.sql("SELECT random() AS value", &[]).unwrap().rows[0]["value"],
        expected_outer.rows[0]["value"]
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SET work_mem = '12MB'", &[]).unwrap();
    engine.sql("SELECT setseed(0.5)", &[]).unwrap();
    engine.sql("SAVEPOINT session_point", &[]).unwrap();

    let savepoint_baseline = Engine::new();
    savepoint_baseline.sql("SELECT setseed(0.5)", &[]).unwrap();
    let expected_savepoint = savepoint_baseline
        .sql("SELECT random() AS value", &[])
        .unwrap();

    engine.sql("SET search_path TO public", &[]).unwrap();
    engine.sql("SET work_mem = '20MB'", &[]).unwrap();
    engine.sql("SELECT setseed(-0.75)", &[]).unwrap();
    engine
        .sql("PREPARE after_savepoint AS SELECT 2 AS value", &[])
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT session_point", &[])
        .unwrap();

    assert_eq!(engine.search_path(), vec!["base", "public"]);
    assert_eq!(
        engine.sql("SHOW work_mem", &[]).unwrap().rows[0]["work_mem"],
        uqa_core::Value::Str("12MB".into())
    );
    let execute_error = engine.sql("EXECUTE after_savepoint", &[]).unwrap_err();
    let aborted = engine.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(aborted.sqlstate(), Some("25P02"), "{execute_error}");
    engine
        .sql("ROLLBACK TO SAVEPOINT session_point", &[])
        .unwrap();
    assert_eq!(
        engine.sql("SELECT random() AS value", &[]).unwrap().rows[0]["value"],
        expected_savepoint.rows[0]["value"]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn persistent_rollback_rebinds_physical_analyzers_from_durable_registry() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("analyzer-rollback.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT); \
             CREATE INDEX docs_body_fts ON docs USING gin (body); \
             INSERT INTO docs (id, body) VALUES (1, 'hello world')",
            &[],
        )
        .unwrap();
    engine
        .register_named_analyzer("whole_value", r#"{"tokenizer":"keyword"}"#)
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
            .unwrap()
            .rows
            .len(),
        1
    );

    engine.begin().unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "whole_value", "both")
        .unwrap();
    assert!(engine
        .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
        .unwrap()
        .rows
        .is_empty());
    engine.rollback().unwrap();

    assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
    assert_eq!(
        engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'hello')", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}
