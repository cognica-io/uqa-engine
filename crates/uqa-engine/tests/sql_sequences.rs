//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE SEQUENCE` / `ALTER SEQUENCE` + `nextval` / `currval` /
//! `setval` round-trips.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::Expr;
use uqa_storage::ManagedConnection;

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
fn sequence_currval_via_sql() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE s2 START 10", &[]).unwrap();
    eng.sql("SELECT nextval('s2') AS v", &[]).unwrap();
    let result = eng.sql("SELECT currval('s2') AS v", &[]).unwrap();
    assert_eq!(result.rows[0]["v"], Value::Int(10));
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
fn column_default_sequence_binding_does_not_follow_search_path_changes() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE SEQUENCE public.ids START 100", &[])
        .unwrap();
    eng.sql("CREATE SEQUENCE app.ids START 10", &[]).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.items (\
             id INTEGER PRIMARY KEY, \
             generated_id INTEGER DEFAULT nextval('ids')\
         )",
        &[],
    )
    .unwrap();

    eng.sql("SET search_path TO public, app", &[]).unwrap();
    eng.sql("INSERT INTO app.items (id) VALUES (1)", &[])
        .unwrap();
    assert_eq!(
        eng.sql("SELECT generated_id FROM app.items WHERE id = 1", &[])
            .unwrap()
            .rows[0]["generated_id"],
        Value::Int(10)
    );
}

#[test]
fn every_sequence_function_default_and_alter_path_stores_a_canonical_target() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    for name in ["next_ids", "current_ids", "set_ids"] {
        eng.sql(&format!("CREATE SEQUENCE app.{name}"), &[])
            .unwrap();
    }
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.defaults (\
             id INTEGER PRIMARY KEY, \
             from_next INTEGER DEFAULT pg_catalog.nextval('next_ids'::regclass), \
             from_current INTEGER DEFAULT currval('current_ids'), \
             from_set INTEGER DEFAULT setval('set_ids', 7)\
         )",
        &[],
    )
    .unwrap();
    assert_eq!(
        default_sequence_reference(&eng, "app.defaults", "from_next"),
        "app.next_ids"
    );
    assert_eq!(
        default_sequence_reference(&eng, "app.defaults", "from_current"),
        "app.current_ids"
    );
    assert_eq!(
        default_sequence_reference(&eng, "app.defaults", "from_set"),
        "app.set_ids"
    );

    eng.sql(
        "ALTER TABLE app.defaults ADD COLUMN added INTEGER DEFAULT nextval('next_ids')",
        &[],
    )
    .unwrap();
    assert_eq!(
        default_sequence_reference(&eng, "app.defaults", "added"),
        "app.next_ids"
    );
    eng.sql(
        "ALTER TABLE app.defaults ALTER COLUMN added SET DEFAULT currval('current_ids')",
        &[],
    )
    .unwrap();
    assert_eq!(
        default_sequence_reference(&eng, "app.defaults", "added"),
        "app.current_ids"
    );
}

#[test]
fn canonical_default_dependency_survives_reopen_and_blocks_only_its_sequence_drop() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("default-sequence-dependency.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("CREATE SEQUENCE public.ids START 100", &[])
            .unwrap();
        eng.sql("CREATE SEQUENCE app.ids START 10", &[]).unwrap();
        eng.sql("SET search_path TO app, public", &[]).unwrap();
        eng.sql(
            "CREATE TABLE app.items (\
                 id INTEGER PRIMARY KEY, \
                 generated_id INTEGER DEFAULT nextval('ids')\
             )",
            &[],
        )
        .unwrap();
    }

    {
        let reopened = Engine::open(&database).unwrap();
        reopened.sql("SET search_path TO public, app", &[]).unwrap();
        assert_eq!(
            default_sequence_reference(&reopened, "app.items", "generated_id"),
            "app.ids"
        );
        assert!(reopened.drop_sequence("ids").unwrap());
        let error = reopened.drop_sequence("app.ids").unwrap_err();
        assert!(error.contains("app.items.generated_id"), "{error}");
        reopened
            .sql("INSERT INTO app.items (id) VALUES (1)", &[])
            .unwrap();
        assert_eq!(
            reopened
                .sql("SELECT generated_id FROM app.items WHERE id = 1", &[],)
                .unwrap()
                .rows[0]["generated_id"],
            Value::Int(10)
        );
        reopened
            .sql(
                "ALTER TABLE app.items ALTER COLUMN generated_id DROP DEFAULT",
                &[],
            )
            .unwrap();
        assert!(reopened.drop_sequence("app.ids").unwrap());
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.sequence_state("app.ids").unwrap().is_none());
    assert!(reopened.sequence_state("public.ids").unwrap().is_none());
}

#[test]
fn sequence_drop_rejects_a_bound_view_dependency() {
    let eng = Engine::new();
    eng.sql("CREATE SEQUENCE ids", &[]).unwrap();
    eng.sql("CREATE VIEW generated AS SELECT nextval('ids') AS id", &[])
        .unwrap();

    let error = eng.drop_sequence("ids").unwrap_err();
    assert!(error.contains("public.generated"), "{error}");
    assert!(eng.sequence_state("ids").unwrap().is_some());
    eng.sql("DROP VIEW generated", &[]).unwrap();
    assert!(eng.drop_sequence("ids").unwrap());
}

#[test]
fn legacy_unqualified_default_sequence_targets_are_unique_or_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-default-sequence.db");
    {
        let eng = Engine::open(&database).unwrap();
        eng.sql("CREATE SCHEMA app", &[]).unwrap();
        eng.sql("CREATE SEQUENCE app.ids START 10", &[]).unwrap();
        eng.sql(
            "CREATE TABLE app.items (\
                 id INTEGER PRIMARY KEY, \
                 generated_id INTEGER DEFAULT nextval('app.ids')\
             )",
            &[],
        )
        .unwrap();
    }
    let connection = ManagedConnection::open(&database).unwrap();
    connection
        .with(|connection| {
            let columns: String = connection.query_row(
                "SELECT columns FROM _tables \
                 WHERE schema_name = 'app' AND relation_name = 'items'",
                [],
                |row| row.get(0),
            )?;
            assert!(columns.contains("app.ids"), "{columns}");
            let legacy = columns.replace("app.ids", "ids");
            connection.execute(
                "UPDATE _tables SET columns = ?1 \
                 WHERE schema_name = 'app' AND relation_name = 'items'",
                [legacy],
            )?;
            Ok(())
        })
        .unwrap();

    let reopened = Engine::open(&database).unwrap();
    reopened.sql("SET search_path TO public", &[]).unwrap();
    assert_eq!(
        default_sequence_reference(&reopened, "app.items", "generated_id"),
        "app.ids"
    );
    reopened
        .sql("INSERT INTO app.items (id) VALUES (1)", &[])
        .unwrap();
    reopened.sql("CREATE SEQUENCE public.ids", &[]).unwrap();
    let ambiguous = reopened
        .column_default_expr("app.items", "generated_id")
        .unwrap_err();
    assert!(
        ambiguous.to_string().contains("ambiguous persisted"),
        "{ambiguous}"
    );
    let insert_error = reopened
        .sql("INSERT INTO app.items (id) VALUES (2)", &[])
        .unwrap_err();
    assert!(
        insert_error.to_string().contains("ambiguous persisted"),
        "{insert_error}"
    );
    drop(reopened);

    connection
        .with(|connection| {
            let columns: String = connection.query_row(
                "SELECT columns FROM _tables \
                 WHERE schema_name = 'app' AND relation_name = 'items'",
                [],
                |row| row.get(0),
            )?;
            let dangling = columns.replace("\"ids\"", "\"missing\"");
            assert_ne!(dangling, columns);
            connection.execute(
                "UPDATE _tables SET columns = ?1 \
                 WHERE schema_name = 'app' AND relation_name = 'items'",
                [dangling],
            )?;
            Ok(())
        })
        .unwrap();
    let dangling = Engine::open(&database).unwrap();
    let error = dangling
        .column_default_expr("app.items", "generated_id")
        .unwrap_err();
    assert!(error.to_string().contains("dangling persisted"), "{error}");
}

#[test]
fn missing_sequence_defaults_fail_without_partial_create_or_alter_state() {
    let eng = Engine::new();
    let create_error = eng
        .sql(
            "CREATE TABLE orphan (\
                 id INTEGER PRIMARY KEY, \
                 generated_id INTEGER DEFAULT nextval('missing')\
             )",
            &[],
        )
        .unwrap_err();
    assert!(
        create_error.to_string().contains("missing"),
        "{create_error}"
    );
    assert!(!eng.has_table("orphan").unwrap());

    eng.sql("CREATE TABLE kept (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let add_error = eng
        .sql(
            "ALTER TABLE kept ADD COLUMN generated_id INTEGER DEFAULT nextval('missing')",
            &[],
        )
        .unwrap_err();
    assert!(add_error.to_string().contains("missing"), "{add_error}");
    assert!(!eng.table_has_column("kept", "generated_id").unwrap());
    let set_error = eng
        .sql(
            "ALTER TABLE kept ALTER COLUMN id SET DEFAULT nextval('missing')",
            &[],
        )
        .unwrap_err();
    assert!(set_error.to_string().contains("missing"), "{set_error}");
    assert!(eng.column_default_expr("kept", "id").unwrap().is_none());
}

#[test]
fn rejected_sequence_ddl_has_no_current_or_reopened_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sequence_rejections.db");
    {
        let eng = Engine::open(&db).unwrap();
        assert!(eng
            .sql("CREATE SEQUENCE rejected INCREMENT BY 0", &[])
            .is_err());
        assert!(eng.sequence_state("rejected").unwrap().is_none());

        eng.sql("CREATE SEQUENCE kept START 10 INCREMENT BY 2", &[])
            .unwrap();
        assert!(eng
            .sql("ALTER SEQUENCE kept INCREMENT BY 0 RESTART WITH 99", &[])
            .is_err());
        assert_eq!(eng.nextval("kept").unwrap(), 10);
        for sql in [
            "SELECT nextval('kept', 'ignored')",
            "SELECT currval('kept', 'ignored')",
            "SELECT setval('kept', 99, true)",
        ] {
            assert!(
                eng.sql(sql, &[]).is_err(),
                "surplus arguments accepted: {sql}"
            );
        }
        assert_eq!(
            eng.sequence_state("kept").unwrap().unwrap().1.current,
            10,
            "rejected sequence calls changed persistent state"
        );

        eng.sql(
            "ALTER SEQUENCE IF EXISTS missing RESTART WITH 50 INCREMENT BY 3",
            &[],
        )
        .unwrap();
        assert!(eng.sequence_state("missing").unwrap().is_none());
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.sequence_state("rejected").unwrap().is_none());
    assert!(reopened.sequence_state("missing").unwrap().is_none());
    assert_eq!(reopened.nextval("kept").unwrap(), 12);
}

#[test]
fn bigint_boundary_starts_and_restarts_do_not_need_a_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sequence_boundaries.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            &format!(
                "CREATE SEQUENCE ascending START WITH {} INCREMENT BY 1",
                i64::MIN
            ),
            &[],
        )
        .unwrap();
        eng.sql(
            &format!(
                "CREATE SEQUENCE descending START WITH {} INCREMENT BY -1",
                i64::MAX
            ),
            &[],
        )
        .unwrap();
        assert_eq!(eng.nextval("ascending").unwrap(), i64::MIN);
        assert_eq!(eng.nextval("descending").unwrap(), i64::MAX);

        eng.sql("CREATE SEQUENCE restartable", &[]).unwrap();
        eng.sql(
            &format!("ALTER SEQUENCE restartable RESTART WITH {}", i64::MIN),
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
