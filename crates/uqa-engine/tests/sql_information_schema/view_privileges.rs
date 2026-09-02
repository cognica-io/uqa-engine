//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn execute(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error
            .sqlstate()
            .expect("failure should expose SQLSTATE")
            .to_string(),
        error.to_string(),
    )
}

fn sqlstate(engine: &Engine, sql: &str) -> String {
    failure(engine, sql).0
}

fn setup(engine: &Engine) {
    for sql in [
        "CREATE ROLE view_acl_owner",
        "CREATE ROLE view_acl_reader",
        "CREATE ROLE view_acl_column_reader",
        "CREATE ROLE view_acl_column_writer",
        "CREATE ROLE view_acl_maintainer",
        "CREATE ROLE view_acl_delegate",
        "CREATE ROLE view_acl_delegated_reader",
        "CREATE ROLE view_acl_all",
        "CREATE ROLE view_acl_outsider",
        "GRANT CREATE ON DATABASE uqa TO view_acl_owner",
        "SET ROLE view_acl_owner",
        "CREATE SCHEMA view_acl",
        "GRANT USAGE ON SCHEMA view_acl TO view_acl_reader, view_acl_column_reader, view_acl_column_writer, view_acl_maintainer, view_acl_delegate, view_acl_delegated_reader, view_acl_all, view_acl_outsider",
        "CREATE TABLE view_acl.base(id integer PRIMARY KEY, value integer)",
        "INSERT INTO view_acl.base VALUES (1, 10), (2, 20)",
        "CREATE VIEW view_acl.items AS SELECT id, value FROM view_acl.base",
        "CREATE VIEW view_acl.identity_items AS SELECT id, current_user AS who FROM view_acl.base",
        "CREATE VIEW view_acl.invoker_items WITH (security_invoker=true) AS SELECT id, value FROM view_acl.base",
        "CREATE VIEW view_acl.writable AS SELECT id, value FROM view_acl.base",
        "CREATE VIEW view_acl.nested_writable AS SELECT id, value FROM view_acl.writable",
        "CREATE VIEW view_acl.column_writable AS SELECT id, value FROM view_acl.base",
        "CREATE VIEW view_acl.invoker_writable WITH (security_invoker=true) AS SELECT id, value FROM view_acl.base",
        "CREATE VIEW view_acl.all_items AS SELECT id, value FROM view_acl.base",
        "CREATE MATERIALIZED VIEW view_acl.snapshot AS SELECT id, value FROM view_acl.base",
        "CREATE MATERIALIZED VIEW view_acl.identity_snapshot AS SELECT current_user AS who",
        "CREATE MATERIALIZED VIEW view_acl.empty_snapshot AS SELECT id FROM view_acl.base WITH NO DATA",
        "CREATE SEQUENCE view_acl.ids",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

fn grant_catalog_acl_fixture(engine: &Engine) {
    for sql in [
        "SET ROLE view_acl_owner",
        "GRANT SELECT(id) ON TABLE view_acl.items TO view_acl_column_reader",
        "GRANT SELECT ON TABLE view_acl.snapshot TO view_acl_reader",
        "GRANT ALL PRIVILEGES ON TABLE view_acl.all_items TO view_acl_all",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

#[test]
fn view_acl_catalog_and_inquiry_surface_matches_pg18() {
    let engine = Engine::new();
    setup(&engine);
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM pg_catalog.pg_class WHERE oid IN ('view_acl.items'::regclass, 'view_acl.snapshot'::regclass) AND relacl IS NULL",
        ),
        Value::Int(2)
    );
    grant_catalog_acl_fixture(&engine);
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'view_acl.items'::regclass AND attname = 'id'",
        ),
        Value::Str("{view_acl_column_reader=r/view_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'view_acl.all_items'::regclass",
        ),
        Value::Str("{view_acl_owner=arwdDxtm/view_acl_owner,view_acl_all=arwdDxtm/view_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('view_acl_column_reader'::regrole::oid, 'view_acl.items'::regclass, 1::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('view_acl_reader', 'view_acl.snapshot', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('view_acl_reader'::regrole::oid, 'view_acl.snapshot'::regclass::oid, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('view_acl_reader'::regrole::oid, 'view_acl.snapshot'::regclass::oid, 1::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('view_acl_column_reader', 'view_acl.items', -1::smallint, 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_column_privilege('view_acl_column_reader', 'view_acl.items', 'ctid', 'SELECT')",
        ),
        "42703"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'view_acl' AND table_name = 'snapshot'",
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.column_privileges WHERE table_schema = 'view_acl' AND table_name = 'snapshot'",
        ),
        Value::Int(0)
    );
}

#[test]
fn view_acl_select_and_information_schema_visibility_are_enforced() {
    let engine = Engine::new();
    setup(&engine);
    grant_catalog_acl_fixture(&engine);
    execute(&engine, "SET ROLE view_acl_column_reader");
    assert_eq!(
        scalar(&engine, "SELECT id FROM view_acl.items"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM view_acl.items"),
        Value::Int(2)
    );
    assert_eq!(
        sqlstate(&engine, "SELECT value FROM view_acl.items"),
        "42501"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'view_acl' AND table_name = 'items'",
        ),
        Value::Str("id".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(column_name || ':' || privilege_type, ',' ORDER BY column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'view_acl' AND table_name = 'items'",
        ),
        Value::Str("id:SELECT".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.views WHERE table_schema = 'view_acl' AND table_name = 'items'",
        ),
        Value::Int(1)
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_outsider");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.views WHERE table_schema = 'view_acl'",
        ),
        Value::Int(0)
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn regular_views_use_definer_privileges_unless_security_invoker_is_set() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE view_acl_owner",
        "GRANT SELECT ON TABLE view_acl.items, view_acl.identity_items, view_acl.invoker_items TO view_acl_reader",
        "RESET ROLE",
        "SET ROLE view_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "SELECT value FROM view_acl.items WHERE id = 1"),
        Value::Int(10)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT who FROM view_acl.identity_items WHERE id = 1"
        ),
        Value::Str("view_acl_reader".into())
    );
    let (state, message) = failure(&engine, "SELECT id FROM view_acl.invoker_items");
    assert_eq!(state, "42501");
    assert!(message.contains("permission denied for table base"));
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_owner");
    execute(
        &engine,
        "GRANT SELECT ON TABLE view_acl.base TO view_acl_reader",
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_reader");
    assert_eq!(
        scalar(&engine, "SELECT id FROM view_acl.invoker_items"),
        Value::Int(1)
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn automatically_updatable_views_preserve_the_relation_privilege_subject() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE view_acl_owner",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE view_acl.writable TO view_acl_reader",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE view_acl.invoker_writable TO view_acl_reader",
        "RESET ROLE",
        "SET ROLE view_acl_reader",
        "INSERT INTO view_acl.writable VALUES (3, 30)",
        "UPDATE view_acl.writable SET value = 31 WHERE id = 3",
        "DELETE FROM view_acl.writable WHERE id = 2",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "INSERT INTO view_acl.invoker_writable VALUES (4, 40)"
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(id::text || ':' || value::text, ',' ORDER BY id) FROM view_acl.base"
        ),
        Value::Str("1:10,3:31".into())
    );
}

#[test]
fn nested_column_sensitive_merge_and_instead_trigger_view_privileges_are_enforced() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE view_acl_owner",
        "GRANT INSERT(id, value), UPDATE(value), SELECT(id) ON TABLE view_acl.column_writable TO view_acl_column_writer",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE view_acl.nested_writable TO view_acl_reader",
        "CREATE TABLE view_acl.trigger_log(id integer)",
        "CREATE VIEW view_acl.triggered AS SELECT id, value FROM view_acl.base",
        "CREATE FUNCTION view_acl.record_view_insert() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN INSERT INTO view_acl.trigger_log VALUES (NEW.id); RETURN NEW; END $$",
        "CREATE TRIGGER record_view_insert INSTEAD OF INSERT ON view_acl.triggered FOR EACH ROW EXECUTE FUNCTION view_acl.record_view_insert()",
        "GRANT INSERT ON TABLE view_acl.triggered TO view_acl_reader",
        "RESET ROLE",
        "SET ROLE view_acl_column_writer",
        "INSERT INTO view_acl.column_writable VALUES (4, 40)",
        "UPDATE view_acl.column_writable SET value = 41 WHERE id = 4",
        "MERGE INTO view_acl.column_writable AS target USING (VALUES (5, 50)) AS source(id, value) ON false WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
        "MERGE INTO view_acl.column_writable AS target USING (VALUES (4, 42)) AS source(id, value) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "UPDATE view_acl.column_writable SET value = value + 1 WHERE id = 4",
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_reader");
    for sql in [
        "INSERT INTO view_acl.nested_writable VALUES (6, 60)",
        "UPDATE view_acl.nested_writable SET value = 61 WHERE id = 6",
        "DELETE FROM view_acl.nested_writable WHERE id = 6",
        "INSERT INTO view_acl.triggered VALUES (7, 70)",
    ] {
        execute(&engine, sql);
    }
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_outsider");
    assert_eq!(
        sqlstate(&engine, "INSERT INTO view_acl.triggered VALUES (8, 80)"),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM view_acl.trigger_log"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(id::text || ':' || value::text, ',' ORDER BY id) FROM view_acl.base WHERE id IN (4, 5)",
        ),
        Value::Str("4:42,5:50".into())
    );
}

#[test]
fn nested_instead_rules_stop_privilege_descent_with_the_original_mutation() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE nested_rule_owner",
        "CREATE ROLE nested_rule_stop_owner",
        "CREATE ROLE nested_rule_caller",
        "GRANT CREATE ON DATABASE uqa TO nested_rule_owner",
        "SET ROLE nested_rule_owner",
        "CREATE SCHEMA nested_rule_acl",
        "GRANT USAGE ON SCHEMA nested_rule_acl TO nested_rule_stop_owner, nested_rule_caller",
        "CREATE TABLE nested_rule_acl.base(id integer PRIMARY KEY, value integer)",
        "INSERT INTO nested_rule_acl.base VALUES (1, 10)",
        "CREATE VIEW nested_rule_acl.low AS SELECT id, value FROM nested_rule_acl.base",
        "CREATE VIEW nested_rule_acl.stop AS SELECT id, value FROM nested_rule_acl.low",
        "CREATE RULE stop_insert AS ON INSERT TO nested_rule_acl.stop DO INSTEAD NOTHING",
        "CREATE RULE stop_update AS ON UPDATE TO nested_rule_acl.stop DO INSTEAD NOTHING",
        "CREATE RULE stop_delete AS ON DELETE TO nested_rule_acl.stop DO INSTEAD NOTHING",
        "CREATE VIEW nested_rule_acl.top AS SELECT id, value FROM nested_rule_acl.stop",
        "GRANT INSERT, UPDATE, DELETE ON TABLE nested_rule_acl.top TO nested_rule_caller",
        "RESET ROLE",
        "ALTER VIEW nested_rule_acl.low OWNER TO nested_rule_stop_owner",
        "ALTER VIEW nested_rule_acl.stop OWNER TO nested_rule_stop_owner",
        "SET ROLE nested_rule_caller",
    ] {
        execute(&engine, sql);
    }
    for sql in [
        "INSERT INTO nested_rule_acl.top VALUES (2, 20)",
        "UPDATE nested_rule_acl.top SET value = 11",
        "DELETE FROM nested_rule_acl.top",
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap().affected_rows,
            0,
            "{sql} should stop at the nested INSTEAD rule"
        );
    }
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(&engine, "SELECT value FROM nested_rule_acl.base"),
        Value::Int(10)
    );
}

#[test]
fn materialized_view_select_maintain_and_owner_refresh_context_are_independent() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE view_acl_owner",
        "GRANT MAINTAIN ON TABLE view_acl.identity_snapshot TO view_acl_maintainer",
        "GRANT MAINTAIN ON TABLE view_acl.items TO view_acl_maintainer",
        "RESET ROLE",
        "SET ROLE view_acl_maintainer",
        "REFRESH MATERIALIZED VIEW view_acl.identity_snapshot",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(&engine, "SELECT who FROM view_acl.identity_snapshot"),
        "42501"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('view_acl_maintainer', 'view_acl.items', 'MAINTAIN')",
        ),
        Value::Bool(true)
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(&engine, "SELECT who FROM view_acl.identity_snapshot"),
        Value::Str("view_acl_owner".into())
    );
    execute(&engine, "SET ROLE view_acl_reader");
    assert_eq!(
        sqlstate(&engine, "SELECT id FROM view_acl.empty_snapshot"),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_owner");
    execute(
        &engine,
        "GRANT SELECT ON TABLE view_acl.empty_snapshot TO view_acl_reader",
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_reader");
    assert_eq!(
        sqlstate(&engine, "SELECT id FROM view_acl.empty_snapshot"),
        "55000"
    );
    assert_eq!(
        sqlstate(&engine, "INSERT INTO view_acl.snapshot VALUES (3, 30)"),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_owner");
    execute(
        &engine,
        "GRANT INSERT ON TABLE view_acl.snapshot TO view_acl_reader",
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_acl_reader");
    let (state, message) = failure(&engine, "INSERT INTO view_acl.snapshot VALUES (3, 30)");
    assert_eq!(state, "42809");
    assert!(message.contains("cannot change materialized view \"snapshot\""));
}

#[test]
fn view_acl_grant_chains_owner_transfer_schema_grants_and_reopen_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("view-acl.db");
    {
        let engine = Engine::open(&database).unwrap();
        setup(&engine);
        for sql in [
            "CREATE ROLE view_acl_next_owner",
            "GRANT view_acl_next_owner TO view_acl_owner WITH INHERIT FALSE, SET TRUE",
            "SET ROLE view_acl_owner",
            "GRANT CREATE ON SCHEMA view_acl TO view_acl_next_owner",
            "GRANT SELECT ON TABLE view_acl.base TO view_acl_next_owner",
            "GRANT SELECT ON TABLE view_acl.items TO view_acl_delegate WITH GRANT OPTION",
            "RESET ROLE",
            "SET ROLE view_acl_delegate",
            "GRANT SELECT ON TABLE view_acl.items TO view_acl_delegated_reader",
            "RESET ROLE",
            "SET ROLE view_acl_owner",
        ] {
            execute(&engine, sql);
        }
        assert_eq!(
            sqlstate(
                &engine,
                "REVOKE SELECT ON TABLE view_acl.items FROM view_acl_delegate RESTRICT",
            ),
            "2BP01"
        );
        execute(
            &engine,
            "CREATE OR REPLACE VIEW view_acl.items AS SELECT id, value FROM view_acl.base WHERE id > 0",
        );
        execute(
            &engine,
            "ALTER VIEW view_acl.items OWNER TO view_acl_next_owner",
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'view_acl.items'::regclass",
            ),
            Value::Str("{view_acl_next_owner=arwdDxtm/view_acl_next_owner,view_acl_delegate=r*/view_acl_next_owner,view_acl_delegated_reader=r/view_acl_delegate}".into())
        );
        execute(&engine, "RESET ROLE");
        assert_eq!(sqlstate(&engine, "DROP ROLE view_acl_delegate"), "2BP01");
        execute(
            &engine,
            "GRANT SELECT ON ALL TABLES IN SCHEMA view_acl TO view_acl_all",
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT has_table_privilege('view_acl_all', 'view_acl.snapshot', 'SELECT')",
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT has_sequence_privilege('view_acl_all', 'view_acl.ids', 'SELECT')",
            ),
            Value::Bool(false)
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'view_acl.items'::regclass",
        ),
        Value::Str("{view_acl_next_owner=arwdDxtm/view_acl_next_owner,view_acl_delegate=r*/view_acl_next_owner,view_acl_delegated_reader=r/view_acl_delegate,view_acl_all=r/view_acl_next_owner}".into())
    );
    execute(&reopened, "SET ROLE view_acl_delegated_reader");
    assert_eq!(
        scalar(&reopened, "SELECT id FROM view_acl.items"),
        Value::Int(1)
    );
}

#[test]
fn view_acl_changes_follow_transactions_external_refresh_and_temporary_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("view-acl-refresh.db");
    let writer = Engine::open(&database).unwrap();
    setup(&writer);
    let observer = Engine::open(&database).unwrap();
    execute(&writer, "SET ROLE view_acl_owner");
    execute(&writer, "BEGIN");
    execute(
        &writer,
        "GRANT SELECT ON TABLE view_acl.items TO view_acl_reader",
    );
    assert_eq!(
        scalar(
            &writer,
            "SELECT has_table_privilege('view_acl_reader', 'view_acl.items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &observer,
            "SELECT has_table_privilege('view_acl_reader', 'view_acl.items', 'SELECT')",
        ),
        Value::Bool(false)
    );
    execute(&writer, "ROLLBACK");
    for sql in [
        "BEGIN",
        "GRANT SELECT ON TABLE view_acl.items TO view_acl_reader",
        "SAVEPOINT before_view_revoke",
        "REVOKE SELECT ON TABLE view_acl.items FROM view_acl_reader",
        "ROLLBACK TO SAVEPOINT before_view_revoke",
        "COMMIT",
    ] {
        execute(&writer, sql);
    }
    assert_eq!(
        scalar(
            &observer,
            "SELECT has_table_privilege('view_acl_reader', 'view_acl.items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    for sql in [
        "CREATE TEMP VIEW temporary_acl_view AS SELECT id FROM view_acl.base",
        "GRANT SELECT ON TABLE temporary_acl_view TO view_acl_reader",
        "RESET ROLE",
        "SET ROLE view_acl_reader",
    ] {
        execute(&writer, sql);
    }
    assert_eq!(
        scalar(&writer, "SELECT id FROM temporary_acl_view"),
        Value::Int(1)
    );
    execute(&writer, "RESET ROLE");
    drop(writer);
    drop(observer);

    let reopened = Engine::open(&database).unwrap();
    execute(&reopened, "SET ROLE view_acl_reader");
    assert_eq!(
        scalar(&reopened, "SELECT id FROM view_acl.items"),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(&reopened, "SELECT id FROM temporary_acl_view"),
        "42P01"
    );
}

#[test]
fn legacy_view_column_metadata_is_migrated_before_acl_state_can_reference_it() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-view-acl.db");
    let legacy_definition = {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE ROLE legacy_view_acl_reader",
            "CREATE TABLE legacy_view_acl_base(id integer PRIMARY KEY, value integer)",
            "INSERT INTO legacy_view_acl_base VALUES (1, 10)",
            "CREATE VIEW legacy_view_acl_items AS SELECT id, value FROM legacy_view_acl_base",
        ] {
            execute(&engine, sql);
        }
        serde_json::to_string(
            &engine
                .view("legacy_view_acl_items")
                .unwrap()
                .expect("legacy fixture view should exist"),
        )
        .unwrap()
    };
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let mut views = catalog.load_views().unwrap();
        let view = views
            .iter_mut()
            .find(|view| view.relation.qualified_name() == "public.legacy_view_acl_items")
            .unwrap();
        view.definition_json = legacy_definition;
        catalog.save_view(view).unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    execute(
        &engine,
        "GRANT SELECT(id) ON TABLE legacy_view_acl_items TO legacy_view_acl_reader",
    );
    drop(engine);

    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let migrated = catalog
        .load_views()
        .unwrap()
        .into_iter()
        .find(|view| view.relation.qualified_name() == "public.legacy_view_acl_items")
        .unwrap();
    assert!(migrated
        .definition_json
        .contains(r#""output_columns":["id","value"]"#));
    assert!(migrated.column_acls.contains_key("id"));
    drop(catalog);

    let reopened = Engine::open(&database).unwrap();
    execute(&reopened, "SET ROLE legacy_view_acl_reader");
    assert_eq!(
        scalar(&reopened, "SELECT id FROM legacy_view_acl_items"),
        Value::Int(1)
    );
}

#[test]
fn broken_view_acl_grant_chains_are_rejected_during_open() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("broken-view-acl.db");
    {
        let engine = Engine::open(&database).unwrap();
        setup(&engine);
        execute(&engine, "SET ROLE view_acl_owner");
        execute(
            &engine,
            "GRANT SELECT ON TABLE view_acl.items TO view_acl_reader",
        );
    }
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let mut views = catalog.load_views().unwrap();
        let view = views
            .iter_mut()
            .find(|view| view.relation.qualified_name() == "view_acl.items")
            .unwrap();
        view.acl
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry.role == "view_acl_reader")
            .unwrap()
            .grantor = Some("view_acl_delegate".into());
        catalog.save_view(view).unwrap();
    }

    let Err(error) = Engine::open(&database) else {
        panic!("broken view ACL should prevent the engine from opening");
    };
    assert!(error.to_string().contains("is not rooted at owner"));
}
