//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_rolled_back_recreation_keeps_the_original_sequence(engine: &Engine) {
    engine
        .create_sequence("incarnation_ids", 10, 1, false)
        .unwrap();
    assert_eq!(engine.nextval("incarnation_ids").unwrap(), 10);
    assert_eq!(engine.lastval().unwrap(), 10);

    engine.begin().unwrap();
    assert!(engine.drop_sequence("incarnation_ids").unwrap());
    engine.rollback().unwrap();
    assert_eq!(engine.currval("incarnation_ids").unwrap(), 10);
    assert_eq!(engine.lastval().unwrap(), 10);

    engine.begin().unwrap();
    assert!(engine.drop_sequence("incarnation_ids").unwrap());
    assert!(engine
        .create_sequence("incarnation_ids", 100, 1, false)
        .unwrap());
    assert_eq!(engine.nextval("incarnation_ids").unwrap(), 100);
    engine.rollback().unwrap();

    assert_eq!(engine.currval("incarnation_ids").unwrap(), 10);
    assert!(engine.lastval().is_err());
    assert_eq!(engine.nextval("incarnation_ids").unwrap(), 11);
    assert_eq!(engine.lastval().unwrap(), 11);
}

#[test]
fn nontransactional_values_do_not_cross_sequence_incarnations() {
    assert_rolled_back_recreation_keeps_the_original_sequence(&Engine::new());

    let directory = tempfile::tempdir().unwrap();
    let persistent = Engine::open(&directory.path().join("sequence-incarnation.sqlite")).unwrap();
    assert_rolled_back_recreation_keeps_the_original_sequence(&persistent);
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
fn serial_ownership_does_not_block_drop_after_default_replacement() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE serial_owner (id SERIAL, marker INTEGER)", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE serial_owner ALTER COLUMN id SET DEFAULT 42",
        &[],
    )
    .unwrap();
    assert!(eng.drop_sequence("public.serial_owner_id_seq").unwrap());
    assert_eq!(
        eng.sql(
            "INSERT INTO serial_owner(marker) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap()
        .rows[0]["id"],
        Value::Int(42)
    );
}

#[test]
fn drop_sequence_sql_matches_missing_wrong_kind_and_multi_target_semantics() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE first_ids", &[]).unwrap();
    engine.sql("CREATE SEQUENCE second_ids", &[]).unwrap();
    engine
        .sql("CREATE TABLE not_a_sequence(id integer)", &[])
        .unwrap();

    let missing = engine.sql("DROP SEQUENCE missing_ids", &[]).unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42P01"));
    engine
        .sql("DROP SEQUENCE IF EXISTS missing_ids", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".to_string(),
            "sequence \"missing_ids\" does not exist, skipping".to_string()
        )]
    );

    for sql in [
        "DROP SEQUENCE not_a_sequence",
        "DROP SEQUENCE IF EXISTS not_a_sequence",
    ] {
        let wrong_kind = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(wrong_kind.sqlstate(), Some("42809"));
    }

    let multi = engine
        .sql("DROP SEQUENCE first_ids, missing_ids", &[])
        .unwrap_err();
    assert_eq!(multi.sqlstate(), Some("42P01"));
    assert!(engine.sequence_state("first_ids").unwrap().is_some());

    engine
        .sql(
            "DROP SEQUENCE IF EXISTS first_ids, missing_ids, first_ids",
            &[],
        )
        .unwrap();
    assert!(engine.sequence_state("first_ids").unwrap().is_none());
    assert!(engine.sequence_state("second_ids").unwrap().is_some());
}

#[test]
fn drop_sequence_sql_restricts_or_cascades_column_and_view_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("drop-sequence-cascade.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine.sql("CREATE SEQUENCE dependency_ids", &[]).unwrap();
        engine
            .sql(
                "CREATE TABLE dependent_rows(id bigint DEFAULT nextval('dependency_ids'))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW dependent_values AS SELECT nextval('dependency_ids') AS id",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW nested_dependent_values AS SELECT id FROM dependent_values",
                &[],
            )
            .unwrap();

        let restricted = engine.sql("DROP SEQUENCE dependency_ids", &[]).unwrap_err();
        assert_eq!(restricted.sqlstate(), Some("2BP01"));
        assert!(engine.sequence_state("dependency_ids").unwrap().is_some());
        assert!(engine
            .column_default_expr("dependent_rows", "id")
            .unwrap()
            .is_some());
        assert!(engine.view("dependent_values").unwrap().is_some());

        engine
            .sql("DROP SEQUENCE dependency_ids CASCADE", &[])
            .unwrap();
        assert!(engine.sequence_state("dependency_ids").unwrap().is_none());
        assert!(engine
            .column_default_expr("dependent_rows", "id")
            .unwrap()
            .is_none());
        assert!(engine.view("dependent_values").unwrap().is_none());
        assert!(engine.view("nested_dependent_values").unwrap().is_none());
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.sequence_state("dependency_ids").unwrap().is_none());
    assert!(reopened
        .column_default_expr("dependent_rows", "id")
        .unwrap()
        .is_none());
    assert!(reopened.view("dependent_values").unwrap().is_none());
    assert!(reopened.view("nested_dependent_values").unwrap().is_none());
}

#[test]
fn drop_sequence_sql_cascade_detaches_serial_default_and_ownership() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE serial_rows(id serial, marker integer)", &[])
        .unwrap();
    let restricted = engine
        .sql("DROP SEQUENCE serial_rows_id_seq", &[])
        .unwrap_err();
    assert_eq!(restricted.sqlstate(), Some("2BP01"));

    engine
        .sql("DROP SEQUENCE serial_rows_id_seq CASCADE", &[])
        .unwrap();
    assert!(engine
        .sequence_state("serial_rows_id_seq")
        .unwrap()
        .is_none());
    assert!(engine
        .column_default_expr("serial_rows", "id")
        .unwrap()
        .is_none());
    let missing_required_value = engine
        .sql(
            "INSERT INTO serial_rows(marker) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_required_value.sqlstate(), Some("23502"));
}

#[test]
fn drop_sequence_sql_is_transactional_and_clears_committed_session_state() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE transactional_ids START 10", &[])
        .unwrap();
    assert_eq!(engine.nextval("transactional_ids").unwrap(), 10);

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("DROP SEQUENCE transactional_ids", &[]).unwrap();
    assert!(engine
        .sequence_state("transactional_ids")
        .unwrap()
        .is_none());
    assert!(engine.lastval().is_err());
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(engine
        .sequence_state("transactional_ids")
        .unwrap()
        .is_some());
    assert_eq!(engine.currval("transactional_ids").unwrap(), 10);
    assert_eq!(engine.lastval().unwrap(), 10);

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SAVEPOINT sequence_drop_point", &[]).unwrap();
    engine.sql("DROP SEQUENCE transactional_ids", &[]).unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT sequence_drop_point", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert!(engine
        .sequence_state("transactional_ids")
        .unwrap()
        .is_some());
    assert_eq!(engine.currval("transactional_ids").unwrap(), 10);
    assert_eq!(engine.lastval().unwrap(), 10);

    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    let read_only = engine
        .sql("DROP SEQUENCE transactional_ids", &[])
        .unwrap_err();
    assert_eq!(read_only.sqlstate(), Some("25006"));
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("DROP SEQUENCE transactional_ids", &[]).unwrap();
    assert!(engine
        .sequence_state("transactional_ids")
        .unwrap()
        .is_none());
    assert!(engine.lastval().is_err());
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
            "SELECT setval('kept', 99, 1)",
            "SELECT setval('kept', 99, true, false)",
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
