//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE SEQUENCE` / `ALTER SEQUENCE` + `nextval` / `currval` / `lastval` / `setval` round-trips.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::Expr;
use uqa_storage::ManagedConnection;

#[path = "sql_sequences/cache.rs"]
mod cache;
#[path = "sql_sequences/introspection.rs"]
mod introspection;
#[path = "sql_sequences/lifecycle.rs"]
mod lifecycle;
#[path = "sql_sequences/name_lifecycle.rs"]
mod name_lifecycle;
#[path = "sql_sequences/ownership.rs"]
mod ownership;
#[path = "sql_sequences/persistence.rs"]
mod persistence;
#[path = "sql_sequences/security.rs"]
mod security;

fn regclass_reference(expression: &Expr) -> Option<&str> {
    match expression {
        Expr::Literal(Value::Str(reference)) => Some(reference),
        Expr::Cast { expr, ty } if ty.ends_with("regclass") => regclass_reference(expr),
        _ => None,
    }
}

fn default_sequence_reference(engine: &Engine, table: &str, column: &str) -> String {
    let expression = engine
        .column_default_expr(table, column)
        .unwrap()
        .expect("column default");
    let Expr::Func { args, .. } = expression else {
        panic!("expected sequence function default, got {expression:?}");
    };
    let Some(reference) = args.first().and_then(regclass_reference) else {
        panic!("expected literal sequence reference, got {args:?}");
    };
    reference.to_string()
}

#[test]
fn sequence_create_and_nextval_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE myseq START 1", &[]).unwrap();
    let first = eng.sql("SELECT nextval('myseq') AS v", &[]).unwrap();
    assert_eq!(first.rows[0]["v"], Value::Int(1));
    let second = eng.sql("SELECT nextval('myseq') AS v", &[]).unwrap();
    assert_eq!(second.rows[0]["v"], Value::Int(2));
}

#[test]
fn insert_default_values_produces_one_row_and_applies_defaults() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE default_value_rows(id BIGSERIAL PRIMARY KEY, payload INTEGER DEFAULT 9)",
            &[],
        )
        .unwrap();

    for expected_id in [1, 2] {
        let result = engine
            .sql(
                "INSERT INTO default_value_rows DEFAULT VALUES RETURNING id, payload",
                &[],
            )
            .unwrap();
        assert_eq!(result.affected_rows, 1);
        assert_eq!(result.rows[0]["id"], Value::Int(expected_id));
        assert_eq!(result.rows[0]["payload"], Value::Int(9));
    }

    engine
        .sql(
            "CREATE TABLE default_identity_rows(id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload INTEGER DEFAULT 11)",
            &[],
        )
        .unwrap();
    let identity = engine
        .sql(
            "INSERT INTO default_identity_rows DEFAULT VALUES RETURNING id, payload",
            &[],
        )
        .unwrap();
    assert_eq!(identity.affected_rows, 1);
    assert_eq!(identity.rows[0]["id"], Value::Int(1));
    assert_eq!(identity.rows[0]["payload"], Value::Int(11));
}

#[test]
fn sequence_currval_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s2 START 10", &[]).unwrap();
    eng.sql("SELECT nextval('s2') AS v", &[]).unwrap();
    let result = eng.sql("SELECT currval('s2') AS v", &[]).unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(10));
}

#[test]
fn lastval_tracks_the_sequence_most_recently_advanced_by_nextval() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE first_ids START 10", &[])
        .unwrap();
    engine
        .sql("CREATE SEQUENCE second_ids START 100", &[])
        .unwrap();

    let undefined = engine.sql("SELECT lastval()", &[]).unwrap_err();
    assert_eq!(undefined.sqlstate(), Some("55000"));
    assert!(undefined
        .to_string()
        .contains("lastval is not yet defined in this session"));

    engine.sql("SELECT setval('first_ids', 20)", &[]).unwrap();
    assert_eq!(
        engine.sql("SELECT lastval()", &[]).unwrap_err().sqlstate(),
        Some("55000")
    );

    assert_eq!(engine.nextval("first_ids").unwrap(), 21);
    assert_eq!(engine.lastval().unwrap(), 21);
    engine.setval("first_ids", 50).unwrap();
    assert_eq!(engine.lastval().unwrap(), 50);

    engine.setval("second_ids", 200).unwrap();
    assert_eq!(engine.currval("second_ids").unwrap(), 200);
    assert_eq!(engine.lastval().unwrap(), 50);
    assert_eq!(engine.nextval("second_ids").unwrap(), 201);
    assert_eq!(engine.lastval().unwrap(), 201);
    engine
        .setval_with_is_called("second_ids", 300, false)
        .unwrap();
    assert_eq!(engine.lastval().unwrap(), 201);
}

#[test]
fn lastval_survives_rollback_and_is_cleared_by_discard_sequences() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE rollback_first START 10", &[])
        .unwrap();
    engine
        .sql("CREATE SEQUENCE rollback_second START 100", &[])
        .unwrap();
    assert_eq!(engine.nextval("rollback_first").unwrap(), 10);

    engine.begin().unwrap();
    assert_eq!(engine.nextval("rollback_second").unwrap(), 100);
    engine.rollback().unwrap();
    assert_eq!(engine.lastval().unwrap(), 100);

    engine.begin().unwrap();
    engine.savepoint("last_value_point").unwrap();
    assert_eq!(engine.nextval("rollback_first").unwrap(), 11);
    engine.rollback_to_savepoint("last_value_point").unwrap();
    assert_eq!(engine.lastval().unwrap(), 11);
    engine.rollback().unwrap();
    assert_eq!(engine.lastval().unwrap(), 11);

    engine.sql("DISCARD SEQUENCES", &[]).unwrap();
    assert_eq!(
        engine.sql("SELECT lastval()", &[]).unwrap_err().sqlstate(),
        Some("55000")
    );
    assert_eq!(
        engine
            .sql("SELECT currval('rollback_first')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
}

#[test]
fn sequence_function_errors_use_postgres_sqlstates() {
    let eng = Engine::new();
    for sql in [
        "SELECT nextval('missing_sequence')",
        "SELECT currval('missing_sequence')",
        "SELECT setval('missing_sequence', 1)",
    ] {
        assert_eq!(eng.sql(sql, &[]).unwrap_err().sqlstate(), Some("42P01"));
    }

    eng.sql("CREATE SEQUENCE untouched_sequence", &[]).unwrap();
    assert_eq!(
        eng.sql("SELECT currval('untouched_sequence')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );

    eng.sql(
        "CREATE SEQUENCE exhausted_sequence START WITH 9223372036854775807",
        &[],
    )
    .unwrap();
    eng.sql("SELECT nextval('exhausted_sequence')", &[])
        .unwrap();
    assert_eq!(
        eng.sql("SELECT nextval('exhausted_sequence')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2200H")
    );
}

#[test]
fn sequence_setval_via_sql_updates_currval() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s3 START 1", &[]).unwrap();
    eng.sql("SELECT nextval('s3') AS v", &[]).unwrap();
    eng.sql("SELECT setval('s3', 100) AS v", &[]).unwrap();
    let result = eng.sql("SELECT currval('s3') AS v", &[]).unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(100));
}

#[test]
fn sequence_function_signatures_match_postgres_overloads() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE signature_sequence", &[]).unwrap();
    assert_eq!(
        eng.sql(
            "SELECT setval('signature_sequence'::text, 5::smallint, false) AS value",
            &[],
        )
        .unwrap()
        .rows[0]["value"],
        Value::Int(5)
    );

    for sql in [
        "SELECT lastval(1)",
        "SELECT setval('signature_sequence', 6, 1)",
        "SELECT setval('signature_sequence', 6.5)",
        "SELECT setval('signature_sequence'::name, 6)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
    }
    let state = eng.sequence_state("signature_sequence").unwrap().unwrap().1;
    assert_eq!(state.current, 5);
    assert!(!state.called);
}

#[test]
fn three_argument_setval_controls_first_allocation_and_session_currval() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE setval_control", &[]).unwrap();

    let result = eng
        .sql("SELECT setval('setval_control', 42, false) AS value", &[])
        .unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(42));
    let state = eng.sequence_state("setval_control").unwrap().unwrap().1;
    assert_eq!(state.current, 42);
    assert!(!state.called);
    assert_eq!(
        eng.sql(
            "SELECT last_value FROM pg_catalog.pg_sequences WHERE sequencename = 'setval_control'",
            &[],
        )
        .unwrap()
        .rows[0]["last_value"],
        Value::Null
    );
    assert_eq!(
        eng.sql("SELECT currval('setval_control')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
    assert_eq!(eng.nextval("setval_control").unwrap(), 42);
    assert_eq!(eng.currval("setval_control").unwrap(), 42);

    assert_eq!(
        eng.sql("SELECT setval('setval_control', 50, true) AS value", &[],)
            .unwrap()
            .rows[0]["value"],
        Value::Int(50)
    );
    assert_eq!(eng.currval("setval_control").unwrap(), 50);
    assert_eq!(eng.nextval("setval_control").unwrap(), 51);
    assert_eq!(
        eng.sql("SELECT setval('setval_control', 70, false) AS value", &[],)
            .unwrap()
            .rows[0]["value"],
        Value::Int(70)
    );
    assert_eq!(eng.currval("setval_control").unwrap(), 51);
    assert_eq!(eng.nextval("setval_control").unwrap(), 70);

    let before_nulls = eng.sequence_state("setval_control").unwrap().unwrap().1;
    let nulls = eng
        .sql(
            "SELECT setval(NULL, 80, false) AS null_name,
                    setval('setval_control', NULL, false) AS null_value,
                    setval('setval_control', 80, NULL) AS null_called",
            &[],
        )
        .unwrap();
    for column in ["null_name", "null_value", "null_called"] {
        assert_eq!(nulls.rows[0][column], Value::Null, "{column}");
    }
    let after_nulls = eng.sequence_state("setval_control").unwrap().unwrap().1;
    assert_eq!(after_nulls.current, before_nulls.current);
    assert_eq!(after_nulls.called, before_nulls.called);
}

#[test]
fn sequence_value_functions_enforce_relation_kind_and_default_bounds() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE not_a_sequence (id INTEGER)", &[])
        .unwrap();
    for sql in [
        "SELECT nextval('not_a_sequence')",
        "SELECT currval('not_a_sequence')",
        "SELECT setval('not_a_sequence', 1)",
        "SELECT setval('not_a_sequence', 1, false)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"), "{sql}: {error}");
    }

    eng.sql("CREATE SEQUENCE ascending_bounds", &[]).unwrap();
    for sql in [
        "SELECT setval('ascending_bounds', 0)",
        "SELECT setval('ascending_bounds', 0, false)",
        "SELECT setval('ascending_bounds', -1, true)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"), "{sql}: {error}");
    }
    let ascending = eng.sequence_state("ascending_bounds").unwrap().unwrap().1;
    assert_eq!(ascending.current, 1);
    assert!(!ascending.called);
    assert_eq!(
        eng.setval_with_is_called("ascending_bounds", i64::MAX, false)
            .unwrap(),
        i64::MAX
    );
    assert_eq!(eng.nextval("ascending_bounds").unwrap(), i64::MAX);
    assert_eq!(
        eng.sql("SELECT nextval('ascending_bounds')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2200H")
    );

    eng.sql("CREATE SEQUENCE descending_bounds INCREMENT BY -1", &[])
        .unwrap();
    assert_eq!(
        eng.sql("SELECT setval('descending_bounds', 0, false)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        eng.setval_with_is_called("descending_bounds", i64::MIN, false)
            .unwrap(),
        i64::MIN
    );
    assert_eq!(eng.nextval("descending_bounds").unwrap(), i64::MIN);
    assert!(eng.nextval("descending_bounds").is_err());
}

#[test]
fn uncalled_setval_state_survives_failed_statements_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("setval-is-called.sqlite");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql("CREATE SEQUENCE durable_setval", &[]).unwrap();
        assert_eq!(eng.nextval("durable_setval").unwrap(), 1);
        let error = eng
            .sql(
                "DO $$
                 BEGIN
                     PERFORM setval('durable_setval', 80, false);
                     RAISE EXCEPTION 'setval statement failure';
                 END
                 $$",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("P0001"));
        let state = eng.sequence_state("durable_setval").unwrap().unwrap().1;
        assert_eq!(state.current, 80);
        assert!(!state.called);
        assert_eq!(eng.currval("durable_setval").unwrap(), 1);

        eng.sql(
            "DO $$
             BEGIN
                 BEGIN
                     PERFORM setval('durable_setval', 90, false);
                     RAISE EXCEPTION 'caught setval failure';
                 EXCEPTION WHEN OTHERS THEN
                     NULL;
                 END;
             END
             $$",
            &[],
        )
        .unwrap();
        let state = eng.sequence_state("durable_setval").unwrap().unwrap().1;
        assert_eq!(state.current, 90);
        assert!(!state.called);
        assert_eq!(eng.currval("durable_setval").unwrap(), 1);
    }

    let reopened = Engine::open(&database).unwrap();
    let state = reopened
        .sequence_state("durable_setval")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(state.current, 90);
    assert!(!state.called);
    assert!(reopened.currval("durable_setval").is_err());
    assert_eq!(
        reopened
            .sql(
                "SELECT last_value FROM pg_catalog.pg_sequences WHERE sequencename = 'durable_setval'",
                &[],
            )
            .unwrap()
            .rows[0]["last_value"],
        Value::Null
    );
    assert_eq!(reopened.nextval("durable_setval").unwrap(), 90);
}

#[test]
fn pg_sequences_reports_sequence_state_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("pg-sequences.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE SEQUENCE catalog_sequence START 10 INCREMENT 5;
                 CREATE SEQUENCE descending_sequence START -2 INCREMENT -3",
                &[],
            )
            .unwrap();
        let before = engine
            .sql(
                "SELECT start_value, min_value, max_value, increment_by, cycle, cache_size, last_value
                 FROM pg_catalog.pg_sequences WHERE sequencename = 'catalog_sequence'",
                &[],
            )
            .unwrap();
        assert_eq!(before.rows[0]["start_value"], Value::Int(10));
        assert_eq!(before.rows[0]["min_value"], Value::Int(1));
        assert_eq!(before.rows[0]["max_value"], Value::Int(i64::MAX));
        assert_eq!(before.rows[0]["increment_by"], Value::Int(5));
        assert_eq!(before.rows[0]["cycle"], Value::Bool(false));
        assert_eq!(before.rows[0]["cache_size"], Value::Int(1));
        assert_eq!(before.rows[0]["last_value"], Value::Null);
        let descending = engine
            .sql(
                "SELECT start_value, min_value, max_value, increment_by
                 FROM pg_catalog.pg_sequences WHERE sequencename = 'descending_sequence'",
                &[],
            )
            .unwrap();
        assert_eq!(descending.rows[0]["start_value"], Value::Int(-2));
        assert_eq!(descending.rows[0]["min_value"], Value::Int(i64::MIN));
        assert_eq!(descending.rows[0]["max_value"], Value::Int(-1));
        assert_eq!(descending.rows[0]["increment_by"], Value::Int(-3));
        assert_eq!(engine.nextval("catalog_sequence").unwrap(), 10);
    }

    let reopened = Engine::open(&database).unwrap();
    let after = reopened
        .sql(
            "SELECT last_value FROM pg_catalog.pg_sequences
             WHERE sequencename = 'catalog_sequence'",
            &[],
        )
        .unwrap();
    assert_eq!(after.rows[0]["last_value"], Value::Int(10));
}

#[test]
fn sequence_declared_bounds_types_and_cycle_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-options.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE SEQUENCE configured AS integer INCREMENT 3 MINVALUE 2 MAXVALUE 10 START 8 CYCLE;
                 CREATE SEQUENCE small_defaults AS smallint",
                &[],
            )
            .unwrap();
        let configured = engine
            .sql(
                "SELECT data_type, start_value, min_value, max_value, increment_by, cycle, cache_size, last_value FROM pg_sequences WHERE sequencename = 'configured'",
                &[],
            )
            .unwrap();
        assert_eq!(
            configured.rows[0]["data_type"],
            Value::Str("integer".into())
        );
        assert_eq!(configured.rows[0]["start_value"], Value::Int(8));
        assert_eq!(configured.rows[0]["min_value"], Value::Int(2));
        assert_eq!(configured.rows[0]["max_value"], Value::Int(10));
        assert_eq!(configured.rows[0]["increment_by"], Value::Int(3));
        assert_eq!(configured.rows[0]["cycle"], Value::Bool(true));
        assert_eq!(configured.rows[0]["cache_size"], Value::Int(1));
        assert_eq!(configured.rows[0]["last_value"], Value::Null);
        let small = engine
            .sql(
                "SELECT data_type, min_value, max_value FROM pg_sequences WHERE sequencename = 'small_defaults'",
                &[],
            )
            .unwrap();
        assert_eq!(small.rows[0]["data_type"], Value::Str("smallint".into()));
        assert_eq!(small.rows[0]["min_value"], Value::Int(1));
        assert_eq!(small.rows[0]["max_value"], Value::Int(i64::from(i16::MAX)));
        for expected in [8, 2, 5, 8, 2, 5, 8] {
            assert_eq!(engine.nextval("configured").unwrap(), expected);
        }
        let below_minimum = engine
            .sql("SELECT setval('configured', 1)", &[])
            .unwrap_err();
        assert_eq!(below_minimum.sqlstate(), Some("22003"));
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(reopened.nextval("configured").unwrap(), 2);
    let catalog = reopened
        .sql(
            "SELECT data_type, min_value, max_value, cycle, last_value FROM pg_sequences WHERE sequencename = 'configured'",
            &[],
        )
        .unwrap();
    assert_eq!(catalog.rows[0]["data_type"], Value::Str("integer".into()));
    assert_eq!(catalog.rows[0]["min_value"], Value::Int(2));
    assert_eq!(catalog.rows[0]["max_value"], Value::Int(10));
    assert_eq!(catalog.rows[0]["cycle"], Value::Bool(true));
    assert_eq!(catalog.rows[0]["last_value"], Value::Int(2));
}

#[test]
fn noncycling_sequence_overshoot_reports_the_configured_bound() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE bounded INCREMENT 3 MINVALUE 2 MAXVALUE 10 START 8",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("bounded").unwrap(), 8);
    let exhausted = engine.sql("SELECT nextval('bounded')", &[]).unwrap_err();
    assert_eq!(exhausted.sqlstate(), Some("2200H"));
    assert!(exhausted.to_string().contains("maximum value"));
    assert!(exhausted.to_string().contains("(10)"));
    assert_eq!(
        engine.sequence_state("bounded").unwrap().unwrap().1.current,
        8
    );
}

#[test]
fn alter_sequence_options_are_transactional_and_validate_atomically() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE adjustable", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE adjustable AS smallint MINVALUE 2 MAXVALUE 5 START 3 RESTART CYCLE",
            &[],
        )
        .unwrap();
    for expected in [3, 4, 5, 2] {
        assert_eq!(engine.nextval("adjustable").unwrap(), expected);
    }
    let configured = engine.sequence_state("adjustable").unwrap().unwrap().1;

    engine.sql("BEGIN", &[]).unwrap();
    assert_eq!(engine.nextval("adjustable").unwrap(), 3);
    engine
        .sql(
            "ALTER SEQUENCE adjustable AS integer MINVALUE 1 MAXVALUE 20 START 10 RESTART WITH 12 NO CYCLE",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("adjustable").unwrap(), 12);
    engine.sql("ROLLBACK", &[]).unwrap();
    let restored = engine.sequence_state("adjustable").unwrap().unwrap().1;
    assert_eq!(restored.data_type, configured.data_type);
    assert_eq!(restored.min_value, configured.min_value);
    assert_eq!(restored.max_value, configured.max_value);
    assert_eq!(restored.cycle, configured.cycle);
    assert_eq!(restored.current, 3);
    assert_eq!(engine.currval("adjustable").unwrap(), 12);
    assert_eq!(engine.lastval().unwrap(), 12);
    assert_eq!(engine.nextval("adjustable").unwrap(), 4);

    for sql in [
        "ALTER SEQUENCE adjustable INCREMENT 0",
        "ALTER SEQUENCE adjustable MINVALUE 5 MAXVALUE 5",
        "ALTER SEQUENCE adjustable START 1",
        "ALTER SEQUENCE adjustable RESTART WITH 6",
    ] {
        let before = engine.sequence_state("adjustable").unwrap().unwrap().1;
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"), "{sql}: {error}");
        assert_eq!(
            engine.sequence_state("adjustable").unwrap().unwrap().1,
            before
        );
    }

    engine
        .sql("CREATE TABLE not_a_sequence(id integer)", &[])
        .unwrap();
    for sql in [
        "ALTER SEQUENCE not_a_sequence CYCLE",
        "ALTER SEQUENCE IF EXISTS not_a_sequence CYCLE",
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some("42809"));
    }
    engine
        .sql("ALTER SEQUENCE IF EXISTS absent_sequence CYCLE", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".to_string(),
            "relation \"absent_sequence\" does not exist, skipping".to_string()
        )]
    );
}

fn assert_sequence_values_follow_transactional_definition_ownership(engine: &Engine) {
    engine
        .sql(
            "CREATE SEQUENCE altered_before_savepoint MINVALUE 1 MAXVALUE 30 START 1",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("altered_before_savepoint").unwrap(), 1);
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE altered_before_savepoint RESTART WITH 10",
            &[],
        )
        .unwrap();
    engine.sql("SAVEPOINT allocation_boundary", &[]).unwrap();
    assert_eq!(engine.nextval("altered_before_savepoint").unwrap(), 10);
    engine
        .sql("ROLLBACK TO SAVEPOINT allocation_boundary", &[])
        .unwrap();
    let after_savepoint = engine
        .sequence_state("altered_before_savepoint")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        (after_savepoint.current, after_savepoint.called),
        (10, true)
    );
    assert_eq!(engine.currval("altered_before_savepoint").unwrap(), 10);
    assert_eq!(engine.nextval("altered_before_savepoint").unwrap(), 11);
    engine.sql("ROLLBACK", &[]).unwrap();
    let after_outer_rollback = engine
        .sequence_state("altered_before_savepoint")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        (after_outer_rollback.current, after_outer_rollback.called),
        (1, true)
    );
    assert_eq!(engine.currval("altered_before_savepoint").unwrap(), 11);
    assert_eq!(engine.lastval().unwrap(), 11);
    assert_eq!(engine.nextval("altered_before_savepoint").unwrap(), 2);

    engine
        .sql(
            "CREATE SEQUENCE altered_inside_savepoint MINVALUE 1 MAXVALUE 30 START 1",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("altered_inside_savepoint").unwrap(), 1);
    engine.sql("BEGIN", &[]).unwrap();
    assert_eq!(engine.nextval("altered_inside_savepoint").unwrap(), 2);
    engine.sql("SAVEPOINT definition_boundary", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE altered_inside_savepoint RESTART WITH 10",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("altered_inside_savepoint").unwrap(), 10);
    engine
        .sql("ROLLBACK TO SAVEPOINT definition_boundary", &[])
        .unwrap();
    let after_definition_rollback = engine
        .sequence_state("altered_inside_savepoint")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        (
            after_definition_rollback.current,
            after_definition_rollback.called
        ),
        (2, true)
    );
    assert_eq!(engine.currval("altered_inside_savepoint").unwrap(), 10);
    assert_eq!(engine.lastval().unwrap(), 10);
    assert_eq!(engine.nextval("altered_inside_savepoint").unwrap(), 3);
    engine.sql("ROLLBACK", &[]).unwrap();
    let after_transaction = engine
        .sequence_state("altered_inside_savepoint")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        (after_transaction.current, after_transaction.called),
        (3, true)
    );
    assert_eq!(engine.currval("altered_inside_savepoint").unwrap(), 3);
    assert_eq!(engine.nextval("altered_inside_savepoint").unwrap(), 4);
}

#[test]
fn sequence_values_follow_transactional_alter_ownership_in_memory() {
    assert_sequence_values_follow_transactional_definition_ownership(&Engine::new());
}

#[test]
fn sequence_values_follow_transactional_alter_ownership_in_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("sequence-alter-rollback.sqlite")).unwrap();
    assert_sequence_values_follow_transactional_definition_ownership(&engine);
}

#[test]
fn create_sequence_option_errors_precede_catalog_mutation() {
    let engine = Engine::new();
    for (sql, state) in [
        ("CREATE SEQUENCE bad_increment INCREMENT 0", "22023"),
        ("CREATE SEQUENCE bad_bounds MINVALUE 5 MAXVALUE 5", "22023"),
        (
            "CREATE SEQUENCE bad_type_bound AS smallint MAXVALUE 32768",
            "22023",
        ),
        ("CREATE SEQUENCE bad_start MINVALUE 5 START 4", "22023"),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    for name in ["bad_increment", "bad_bounds", "bad_type_bound", "bad_start"] {
        assert!(engine.sequence_state(name).unwrap().is_none(), "{name}");
    }
}

#[test]
fn discard_sequences_clears_currval_without_dropping_definition() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE kept START 3", &[]).unwrap();
    assert_eq!(eng.nextval("kept").unwrap(), 3);
    eng.sql("DISCARD SEQUENCES", &[]).unwrap();
    assert!(eng.currval("kept").is_err());
    assert_eq!(eng.nextval("kept").unwrap(), 4);
}

#[test]
fn sequence_increment_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s4 START 1 INCREMENT 5", &[])
        .unwrap();
    let first = eng.sql("SELECT nextval('s4') AS v", &[]).unwrap();
    assert_eq!(first.rows[0]["v"], Value::Int(1));
    let second = eng.sql("SELECT nextval('s4') AS v", &[]).unwrap();
    assert_eq!(second.rows[0]["v"], Value::Int(6));
}

#[test]
fn create_sequence_default_start_increment_one() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s1", &[]).unwrap();
    assert_eq!(eng.nextval("s1").unwrap(), 1);
    assert_eq!(eng.nextval("s1").unwrap(), 2);
    assert_eq!(eng.currval("s1").unwrap(), 2);
}

#[test]
fn create_sequence_with_options() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s2 START 10 INCREMENT 5", &[])
        .unwrap();
    assert_eq!(eng.nextval("s2").unwrap(), 10);
    assert_eq!(eng.nextval("s2").unwrap(), 15);
}

#[test]
fn alter_sequence_restart_resets_current() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s3 START 100", &[]).unwrap();
    let _ = eng.nextval("s3").unwrap();
    let _ = eng.nextval("s3").unwrap();
    eng.sql("ALTER SEQUENCE s3 RESTART WITH 50", &[]).unwrap();
    assert_eq!(eng.nextval("s3").unwrap(), 50);
}

#[test]
fn nextval_through_select_projection() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s4", &[]).unwrap();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1), (2), (3)", &[])
        .unwrap();
    let result = eng.sql("SELECT nextval('s4') AS n FROM t", &[]).unwrap();
    let ns: Vec<i64> = result
        .rows
        .iter()
        .map(|r| match r.get("n") {
            Some(Value::Int(n)) => *n,
            _ => -1,
        })
        .collect();
    assert_eq!(ns, vec![1, 2, 3]);
}

#[test]
fn setval_overrides_current() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s5", &[]).unwrap();
    let _ = eng.nextval("s5").unwrap();
    let _ = eng.setval("s5", 100).unwrap();
    assert_eq!(eng.nextval("s5").unwrap(), 101);
}

#[test]
fn sequence_state_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sequences.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql("CREATE SEQUENCE app.s START 10 INCREMENT 5", &[])
            .unwrap();
        assert_eq!(
            eng.sql("SELECT nextval('s') AS v", &[]).unwrap().rows[0]["v"],
            Value::Int(10)
        );
    }
    let eng = Engine::open(&db).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    assert_eq!(
        eng.sql("SELECT nextval('s') AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(15)
    );
}

#[test]
fn bigint_boundary_starts_and_restarts_do_not_need_a_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sequence_boundaries.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            &format!(
                "CREATE SEQUENCE ascending START WITH {} INCREMENT BY 1 MINVALUE {}",
                i64::MIN,
                i64::MIN
            ),
            &[],
        )
        .unwrap();
        eng.sql(
            &format!(
                "CREATE SEQUENCE descending START WITH {} INCREMENT BY -1 MAXVALUE {}",
                i64::MAX,
                i64::MAX
            ),
            &[],
        )
        .unwrap();
        assert_eq!(eng.nextval("ascending").unwrap(), i64::MIN);
        assert_eq!(eng.nextval("descending").unwrap(), i64::MAX);

        eng.sql("CREATE SEQUENCE restartable", &[]).unwrap();
        eng.sql(
            &format!(
                "ALTER SEQUENCE restartable MINVALUE {} RESTART WITH {}",
                i64::MIN,
                i64::MIN
            ),
            &[],
        )
        .unwrap();
        assert_eq!(eng.nextval("restartable").unwrap(), i64::MIN);

        eng.sql(
            &format!("CREATE SEQUENCE exhausted START WITH {}", i64::MAX),
            &[],
        )
        .unwrap();
        assert_eq!(eng.nextval("exhausted").unwrap(), i64::MAX);
        assert!(eng.nextval("exhausted").is_err());
        assert_eq!(
            eng.sequence_state("exhausted").unwrap().unwrap().1.current,
            i64::MAX
        );

        eng.sql("CREATE SEQUENCE descending_default INCREMENT BY -2", &[])
            .unwrap();
        assert_eq!(eng.nextval("descending_default").unwrap(), -1);
    }

    let reopened = Engine::open(&db).unwrap();
    assert_eq!(reopened.nextval("ascending").unwrap(), i64::MIN + 1);
    assert_eq!(reopened.nextval("descending").unwrap(), i64::MAX - 1);
    assert_eq!(reopened.nextval("restartable").unwrap(), i64::MIN + 1);
    assert_eq!(reopened.nextval("descending_default").unwrap(), -3);
    assert!(reopened.nextval("exhausted").is_err());
    assert_eq!(
        reopened
            .sequence_state("exhausted")
            .unwrap()
            .unwrap()
            .1
            .current,
        i64::MAX
    );
}
