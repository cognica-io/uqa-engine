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

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn setup_table_acl(engine: &Engine) {
    for sql in [
        "CREATE ROLE table_acl_owner",
        "CREATE ROLE table_acl_all",
        "CREATE ROLE table_acl_select",
        "CREATE ROLE table_acl_insert",
        "CREATE ROLE table_acl_update",
        "CREATE ROLE table_acl_delete",
        "CREATE ROLE table_acl_truncate",
        "CREATE ROLE table_acl_maintain",
        "CREATE ROLE table_acl_references",
        "CREATE ROLE table_acl_outsider",
        "GRANT CREATE ON DATABASE uqa TO table_acl_owner",
        "SET ROLE table_acl_owner",
        "CREATE SCHEMA table_acl",
        "GRANT USAGE ON SCHEMA table_acl TO table_acl_all, table_acl_select, table_acl_insert, table_acl_update, table_acl_delete, table_acl_truncate, table_acl_maintain, table_acl_references, table_acl_outsider",
        "GRANT CREATE ON SCHEMA table_acl TO table_acl_references",
        "CREATE TABLE table_acl.items(id integer PRIMARY KEY, value integer)",
        "CREATE TABLE table_acl.parent(id integer PRIMARY KEY)",
        "CREATE SEQUENCE table_acl.ids",
        "INSERT INTO table_acl.items VALUES (1, 10), (2, 20)",
        "INSERT INTO table_acl.parent VALUES (1)",
        "GRANT ALL PRIVILEGES ON TABLE table_acl.items TO table_acl_all",
        "GRANT SELECT ON TABLE table_acl.items TO table_acl_select",
        "GRANT INSERT ON TABLE table_acl.items TO table_acl_insert",
        "GRANT UPDATE ON TABLE table_acl.items TO table_acl_update",
        "GRANT DELETE ON TABLE table_acl.items TO table_acl_delete",
        "GRANT TRUNCATE ON TABLE table_acl.items TO table_acl_truncate",
        "GRANT MAINTAIN ON TABLE table_acl.items TO table_acl_maintain",
        "GRANT REFERENCES ON TABLE table_acl.parent TO table_acl_references",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

fn assert_table_acl_catalog_inquiry(engine: &Engine) {
    assert_eq!(
        scalar(
            engine,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'table_acl.items'::regclass",
        ),
        Value::Str("{table_acl_owner=arwdDxtm/table_acl_owner,table_acl_all=arwdDxtm/table_acl_owner,table_acl_select=r/table_acl_owner,table_acl_insert=a/table_acl_owner,table_acl_update=w/table_acl_owner,table_acl_delete=d/table_acl_owner,table_acl_truncate=D/table_acl_owner,table_acl_maintain=m/table_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege('table_acl_all', 'table_acl.items', 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER, MAINTAIN')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege('table_acl_outsider', 'table_acl.items', 'SELECT')",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'table_acl_select'), (SELECT oid FROM pg_catalog.pg_class WHERE oid = 'table_acl.items'::regclass), 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege('table_acl_insert', 'table_acl.items', 'SELECT, INSERT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege(4294967290::oid, 'table_acl.items', 'SELECT')",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege(4294967290::oid, 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege(NULL::text, 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(engine, "SELECT has_table_privilege('4294967290', 'SELECT')"),
        "42P01"
    );
    for (oid, source) in [
        (1922, "has_table_privilege_name_name"),
        (1923, "has_table_privilege_name_id"),
        (1924, "has_table_privilege_id_name"),
        (1925, "has_table_privilege_id_id"),
        (1926, "has_table_privilege_name"),
        (1927, "has_table_privilege_id"),
    ] {
        assert_eq!(
            scalar(
                engine,
                &format!("SELECT prosrc FROM pg_catalog.pg_proc WHERE oid = {oid}"),
            ),
            Value::Str(source.into())
        );
    }
}

fn assert_table_acl_dml_enforcement(engine: &Engine) {
    execute(engine, "SET ROLE table_acl_select");
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM table_acl.items"),
        Value::Int(2)
    );
    assert_eq!(
        sqlstate(engine, "INSERT INTO table_acl.items VALUES (3, 30)"),
        "42501"
    );
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_insert");
    assert_eq!(
        sqlstate(
            engine,
            "INSERT INTO table_acl.items VALUES (4, 40) RETURNING id",
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            engine,
            "INSERT INTO table_acl.items VALUES (1, 40) ON CONFLICT (id) DO NOTHING",
        ),
        "42501"
    );
    execute(engine, "INSERT INTO table_acl.items VALUES (3, 30)");
    execute(
        engine,
        "MERGE INTO table_acl.items AS target USING (VALUES (5, 50)) AS source(id, value) ON false WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
    );
    assert_eq!(
        sqlstate(
            engine,
            "MERGE INTO table_acl.items AS target USING (VALUES (6, 60)) AS source(id, value) ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
        ),
        "42501"
    );
    assert_eq!(sqlstate(engine, "SELECT * FROM table_acl.items"), "42501");
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_update");
    assert_eq!(
        sqlstate(engine, "UPDATE table_acl.items SET value = value + 1"),
        "42501"
    );
    assert_eq!(
        sqlstate(engine, "UPDATE table_acl.items SET value = 40 WHERE id = 1"),
        "42501"
    );
    execute(engine, "UPDATE table_acl.items SET value = 40");
    execute(
        engine,
        "MERGE INTO table_acl.items AS target USING (VALUES (0)) AS source(id) ON false WHEN NOT MATCHED BY SOURCE THEN UPDATE SET value = 50",
    );
    assert_eq!(
        sqlstate(
            engine,
            "MERGE INTO table_acl.items AS target USING (VALUES (0)) AS source(id) ON false WHEN NOT MATCHED BY SOURCE THEN UPDATE SET value = target.value + 1",
        ),
        "42501"
    );
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_delete");
    assert_eq!(
        sqlstate(engine, "DELETE FROM table_acl.items WHERE id = 1"),
        "42501"
    );
    execute(engine, "DELETE FROM table_acl.items");
    execute(engine, "RESET ROLE");
}

fn assert_table_acl_maintenance_reference_truncate_and_owner_rights(engine: &Engine) {
    execute(engine, "SET ROLE table_acl_maintain");
    execute(engine, "ANALYZE table_acl.items");
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_references");
    execute(
        engine,
        "CREATE TABLE table_acl.child(id integer REFERENCES table_acl.parent(id))",
    );
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_truncate");
    execute(engine, "TRUNCATE TABLE table_acl.items");
    execute(engine, "RESET ROLE");

    execute(engine, "SET ROLE table_acl_owner");
    execute(
        engine,
        "REVOKE ALL PRIVILEGES ON TABLE table_acl.items FROM table_acl_owner",
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_table_privilege('table_acl.items', 'SELECT WITH GRANT OPTION, MAINTAIN WITH GRANT OPTION')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM table_acl.items"),
        Value::Int(0)
    );
    execute(engine, "RESET ROLE");
}

#[test]
fn table_acl_catalog_inquiry_and_core_enforcement_match_postgresql() {
    let engine = Engine::new();
    setup_table_acl(&engine);
    assert_table_acl_catalog_inquiry(&engine);
    assert_table_acl_dml_enforcement(&engine);
    assert_table_acl_maintenance_reference_truncate_and_owner_rights(&engine);
}

#[test]
fn on_table_sequence_targets_use_sequence_acl_without_table_fallback_state() {
    let engine = Engine::new();
    setup_table_acl(&engine);
    execute(&engine, "SET ROLE table_acl_owner");
    execute(
        &engine,
        "GRANT SELECT, UPDATE ON TABLE table_acl.ids TO table_acl_select",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('table_acl_select', 'table_acl.ids', 'SELECT, UPDATE')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_acl_select', 'table_acl.ids', 'SELECT, UPDATE')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_acl_select', 'table_acl.ids'::regclass, 'DELETE')",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'table_acl.ids'::regclass",
        ),
        Value::Str(
            "{table_acl_owner=rwU/table_acl_owner,table_acl_select=rw/table_acl_owner}".into()
        )
    );
}

#[test]
fn table_acl_follows_transactions_external_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("table-acl.db");
    {
        let writer = Engine::open(&database).unwrap();
        for sql in [
            "CREATE ROLE table_durable_owner",
            "CREATE ROLE table_durable_reader",
            "GRANT CREATE ON SCHEMA public TO table_durable_owner",
            "SET ROLE table_durable_owner",
            "CREATE TABLE table_durable_items(id integer PRIMARY KEY, value text)",
            "INSERT INTO table_durable_items VALUES (1, 'persisted')",
            "RESET ROLE",
        ] {
            execute(&writer, sql);
        }
        let observer = Engine::open(&database).unwrap();
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(false)
        );

        execute(&writer, "SET ROLE table_durable_owner");
        execute(&writer, "BEGIN");
        execute(
            &writer,
            "GRANT SELECT ON TABLE table_durable_items TO table_durable_reader",
        );
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(false)
        );
        execute(&writer, "ROLLBACK");
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(false)
        );

        for sql in [
            "BEGIN",
            "GRANT SELECT ON TABLE table_durable_items TO table_durable_reader",
            "SAVEPOINT before_table_revoke",
            "REVOKE SELECT ON TABLE table_durable_items FROM table_durable_reader",
        ] {
            execute(&writer, sql);
        }
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(false)
        );
        execute(&writer, "ROLLBACK TO SAVEPOINT before_table_revoke");
        execute(&writer, "COMMIT");
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_table_privilege('table_durable_reader', 'table_durable_items', 'SELECT')",
            ),
            Value::Bool(true)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    execute(&reopened, "SET ROLE table_durable_reader");
    assert_eq!(
        scalar(
            &reopened,
            "SELECT value FROM table_durable_items WHERE id = 1",
        ),
        Value::Str("persisted".into())
    );
}

#[test]
fn temporary_table_acl_is_transactional_and_discarded_with_the_relation() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE table_temp_owner",
        "CREATE ROLE table_temp_reader",
        "SET ROLE table_temp_owner",
        "CREATE TEMP TABLE table_temp_items(id integer)",
        "INSERT INTO table_temp_items VALUES (1)",
        "GRANT SELECT ON TABLE table_temp_items TO table_temp_reader",
        "ALTER TABLE table_temp_items RENAME TO table_temp_renamed",
        "RESET ROLE",
        "SET ROLE table_temp_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "SELECT id FROM table_temp_renamed"),
        Value::Int(1)
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE table_temp_owner",
        "BEGIN",
        "REVOKE SELECT ON TABLE table_temp_renamed FROM table_temp_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_temp_reader', 'table_temp_renamed', 'SELECT')",
        ),
        Value::Bool(false)
    );
    execute(&engine, "ROLLBACK");
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_temp_reader', 'table_temp_renamed', 'SELECT')",
        ),
        Value::Bool(true)
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "DISCARD TEMP");
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_table_privilege('table_temp_reader', 'table_temp_renamed', 'SELECT')",
        ),
        "42P01"
    );
}

fn assert_table_acl_target_resolution_and_read_only(engine: &Engine) {
    assert_eq!(
        sqlstate(
            engine,
            "GRANT SELECT ON TABLE missing_table_target TO missing_table_role",
        ),
        "42P01"
    );
    assert_eq!(
        sqlstate(
            engine,
            "GRANT USAGE ON TABLE table_chain_items TO missing_table_role",
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            engine,
            "GRANT USAGE ON TABLE table_chain_items TO table_chain_tail",
        ),
        "0LP01"
    );
    execute(engine, "BEGIN READ ONLY");
    assert_eq!(
        sqlstate(
            engine,
            "GRANT SELECT ON TABLE table_chain_items TO table_chain_tail",
        ),
        "25006"
    );
    execute(engine, "ROLLBACK");
}

#[test]
fn table_acl_grant_paths_owner_transfer_and_target_resolution_are_atomic() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE table_chain_owner",
        "CREATE ROLE table_chain_next_owner",
        "CREATE ROLE table_chain_delegate",
        "CREATE ROLE table_chain_reader",
        "CREATE ROLE table_chain_tail",
        "GRANT CREATE ON SCHEMA public TO table_chain_owner, table_chain_next_owner",
        "GRANT table_chain_next_owner TO table_chain_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE table_chain_owner",
        "CREATE TABLE table_chain_items(id integer)",
        "GRANT SELECT ON TABLE table_chain_items TO table_chain_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE table_chain_delegate",
        "GRANT SELECT ON TABLE table_chain_items TO table_chain_reader WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE table_chain_reader",
        "GRANT SELECT ON TABLE table_chain_items TO table_chain_tail",
        "RESET ROLE",
        "SET ROLE table_chain_owner",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "REVOKE GRANT OPTION FOR SELECT ON TABLE table_chain_items FROM table_chain_delegate RESTRICT",
        ),
        "2BP01"
    );
    execute(
        &engine,
        "REVOKE GRANT OPTION FOR SELECT ON TABLE table_chain_items FROM table_chain_delegate CASCADE",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_chain_delegate', 'table_chain_items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_chain_reader', 'table_chain_items', 'SELECT')",
        ),
        Value::Bool(false)
    );
    execute(
        &engine,
        "GRANT ALL PRIVILEGES ON TABLE table_chain_items TO table_chain_next_owner WITH GRANT OPTION",
    );
    execute(
        &engine,
        "GRANT SELECT ON TABLE table_chain_items TO table_chain_reader",
    );
    execute(
        &engine,
        "ALTER TABLE table_chain_items OWNER TO table_chain_next_owner",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_chain_next_owner', 'table_chain_items', 'SELECT WITH GRANT OPTION, MAINTAIN WITH GRANT OPTION')",
        ),
        Value::Bool(true)
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE table_chain_reader"), "2BP01");
    for sql in [
        "SET ROLE table_chain_next_owner",
        "REVOKE SELECT ON TABLE table_chain_items FROM table_chain_reader",
        "RESET ROLE",
        "DROP ROLE table_chain_reader",
    ] {
        execute(&engine, sql);
    }
    assert_table_acl_target_resolution_and_read_only(&engine);
}

#[test]
fn all_tables_in_schema_excludes_sequences() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE table_schema_owner",
        "CREATE ROLE table_schema_reader",
        "GRANT CREATE ON DATABASE uqa TO table_schema_owner",
        "SET ROLE table_schema_owner",
        "CREATE SCHEMA table_schema_acl",
        "CREATE TABLE table_schema_acl.items(id integer)",
        "CREATE SEQUENCE table_schema_acl.ids",
        "GRANT USAGE ON SCHEMA table_schema_acl TO table_schema_reader",
        "GRANT SELECT ON ALL TABLES IN SCHEMA table_schema_acl TO table_schema_reader",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('table_schema_reader', 'table_schema_acl.items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_sequence_privilege('table_schema_reader', 'table_schema_acl.ids', 'SELECT')",
        ),
        Value::Bool(false)
    );
}

#[test]
fn hierarchy_scans_check_the_named_parent_privilege_only() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE table_hierarchy_owner",
        "CREATE ROLE table_hierarchy_reader",
        "GRANT CREATE ON SCHEMA public TO table_hierarchy_owner",
        "SET ROLE table_hierarchy_owner",
        "CREATE TABLE table_acl_parent(id integer)",
        "CREATE TABLE table_acl_child() INHERITS (table_acl_parent)",
        "CREATE TABLE table_acl_partitioned(id integer) PARTITION BY RANGE(id)",
        "CREATE TABLE table_acl_partition PARTITION OF table_acl_partitioned FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE table_acl_fk_parent(id integer PRIMARY KEY)",
        "CREATE TABLE table_acl_fk_child(id integer REFERENCES table_acl_fk_parent(id))",
        "INSERT INTO table_acl_child VALUES (1)",
        "INSERT INTO table_acl_partitioned VALUES (2)",
        "GRANT SELECT, TRUNCATE ON TABLE table_acl_parent, table_acl_partitioned TO table_hierarchy_reader",
        "GRANT TRUNCATE ON TABLE table_acl_fk_parent TO table_hierarchy_reader",
        "RESET ROLE",
        "SET ROLE table_hierarchy_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM table_acl_parent"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM table_acl_partitioned"),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(&engine, "SELECT count(*) FROM table_acl_child"),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "SELECT count(*) FROM table_acl_partition"),
        "42501"
    );
    execute(&engine, "TRUNCATE TABLE table_acl_parent");
    execute(&engine, "TRUNCATE TABLE table_acl_partitioned");
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM table_acl_parent"),
        Value::Int(0)
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM table_acl_partitioned"),
        Value::Int(0)
    );
    assert_eq!(
        sqlstate(&engine, "TRUNCATE TABLE table_acl_fk_parent CASCADE"),
        "42501"
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE table_hierarchy_owner",
        "GRANT TRUNCATE ON TABLE table_acl_fk_child TO table_hierarchy_reader",
        "RESET ROLE",
        "SET ROLE table_hierarchy_reader",
        "TRUNCATE TABLE table_acl_fk_parent CASCADE",
    ] {
        execute(&engine, sql);
    }
}

#[test]
fn copy_streams_check_table_privileges_before_empty_input_or_output() {
    let engine = Engine::new();
    setup_table_acl(&engine);

    execute(&engine, "SET ROLE table_acl_outsider");
    let error = engine
        .copy_from(
            "COPY table_acl.items FROM STDIN",
            std::io::Cursor::new(Vec::<u8>::new()),
        )
        .expect_err("COPY FROM should require INSERT for empty input");
    assert_eq!(error.sqlstate(), Some("42501"));
    let error = engine
        .copy_to("COPY table_acl.items TO STDOUT", Vec::new())
        .expect_err("COPY TO should require SELECT");
    assert_eq!(error.sqlstate(), Some("42501"));

    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE table_acl_insert");
    assert_eq!(
        engine
            .copy_from(
                "COPY table_acl.items FROM STDIN",
                std::io::Cursor::new(Vec::<u8>::new()),
            )
            .unwrap(),
        0
    );
    let error = engine
        .copy_to("COPY table_acl.items TO STDOUT", Vec::new())
        .expect_err("INSERT does not imply SELECT for COPY TO");
    assert_eq!(error.sqlstate(), Some("42501"));

    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE table_acl_select");
    assert_eq!(
        engine
            .copy_to("COPY table_acl.items TO STDOUT", Vec::new())
            .unwrap(),
        2
    );
}

#[test]
fn catalog_wide_analyze_and_vacuum_skip_tables_without_maintain_privilege() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE maintenance_owner",
        "CREATE ROLE maintenance_worker",
        "GRANT CREATE ON SCHEMA public TO maintenance_owner",
        "SET ROLE maintenance_owner",
        "CREATE TABLE maintenance_allowed(id integer)",
        "CREATE TABLE maintenance_denied(id integer)",
        "GRANT MAINTAIN ON TABLE maintenance_allowed TO maintenance_worker",
        "RESET ROLE",
        "SET ROLE maintenance_worker",
    ] {
        execute(&engine, sql);
    }

    execute(&engine, "ANALYZE");
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "WARNING".into(),
            "permission denied to analyze \"maintenance_denied\", skipping it".into()
        )]
    );
    execute(&engine, "VACUUM (ANALYZE)");
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "WARNING".into(),
            "permission denied to vacuum \"maintenance_denied\", skipping it".into()
        )]
    );
}

#[test]
fn public_table_acl_controls_information_schema_and_non_grantors_only_warn() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE public_acl_owner",
        "CREATE ROLE public_acl_reader",
        "CREATE ROLE public_acl_recipient",
        "GRANT CREATE ON SCHEMA public TO public_acl_owner",
        "SET ROLE public_acl_owner",
        "CREATE TABLE public_acl_items(id integer, value text)",
        "INSERT INTO public_acl_items VALUES (1, 'visible')",
        "RESET ROLE",
        "SET ROLE public_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'public_acl_items'",
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'public_acl_items'",
        ),
        Value::Int(0)
    );
    assert_eq!(sqlstate(&engine, "SELECT * FROM public_acl_items"), "42501");

    for sql in [
        "RESET ROLE",
        "SET ROLE public_acl_owner",
        "GRANT SELECT ON TABLE public_acl_items TO PUBLIC",
        "RESET ROLE",
        "SET ROLE public_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "SELECT value FROM public_acl_items"),
        Value::Str("visible".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'public_acl_items'",
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'public_acl_items'",
        ),
        Value::Int(2)
    );
    execute(
        &engine,
        "GRANT INSERT ON TABLE public_acl_items TO public_acl_recipient",
    );
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "WARNING".into(),
            "no privileges were granted for \"public_acl_items\"".into()
        )]
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('public_acl_recipient', 'public_acl_items', 'INSERT')",
        ),
        Value::Bool(false)
    );

    for sql in [
        "RESET ROLE",
        "SET ROLE public_acl_owner",
        "REVOKE SELECT ON TABLE public_acl_items FROM PUBLIC",
        "RESET ROLE",
        "SET ROLE public_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'public_acl_items'",
        ),
        Value::Int(0)
    );
}

#[test]
fn prepared_queries_observe_revocation_while_declared_cursors_keep_their_acl_snapshot() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE prepared_acl_owner",
        "CREATE ROLE prepared_acl_reader",
        "GRANT CREATE ON SCHEMA public TO prepared_acl_owner",
        "SET ROLE prepared_acl_owner",
        "CREATE TABLE prepared_acl_items(id integer)",
        "INSERT INTO prepared_acl_items VALUES (7)",
        "GRANT SELECT ON TABLE prepared_acl_items TO prepared_acl_reader",
        "RESET ROLE",
        "SET ROLE prepared_acl_reader",
        "PREPARE prepared_acl_query AS SELECT id FROM prepared_acl_items",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(scalar(&engine, "EXECUTE prepared_acl_query"), Value::Int(7));
    for sql in [
        "RESET ROLE",
        "SET ROLE prepared_acl_owner",
        "REVOKE SELECT ON TABLE prepared_acl_items FROM prepared_acl_reader",
        "RESET ROLE",
        "SET ROLE prepared_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(sqlstate(&engine, "EXECUTE prepared_acl_query"), "42501");

    for sql in [
        "RESET ROLE",
        "SET ROLE prepared_acl_owner",
        "GRANT SELECT ON TABLE prepared_acl_items TO prepared_acl_reader",
        "RESET ROLE",
        "SET ROLE prepared_acl_reader",
        "BEGIN",
        "DECLARE prepared_acl_cursor CURSOR FOR SELECT id FROM prepared_acl_items",
        "RESET ROLE",
        "SET ROLE prepared_acl_owner",
        "REVOKE SELECT ON TABLE prepared_acl_items FROM prepared_acl_reader",
        "RESET ROLE",
        "SET ROLE prepared_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(scalar(&engine, "FETCH prepared_acl_cursor"), Value::Int(7));
    execute(&engine, "ROLLBACK");
}

#[test]
fn ordinary_table_trigger_creation_requires_trigger_privilege() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE trigger_acl_owner",
        "CREATE ROLE trigger_acl_creator",
        "GRANT CREATE ON SCHEMA public TO trigger_acl_owner",
        "SET ROLE trigger_acl_owner",
        "CREATE TABLE trigger_acl_items(id integer)",
        "CREATE FUNCTION trigger_acl_passthrough() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "RESET ROLE",
        "SET ROLE trigger_acl_creator",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TRIGGER trigger_acl_denied BEFORE INSERT ON trigger_acl_items FOR EACH ROW EXECUTE FUNCTION trigger_acl_passthrough()",
        ),
        "42501"
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE trigger_acl_owner",
        "GRANT TRIGGER ON TABLE trigger_acl_items TO trigger_acl_creator",
        "RESET ROLE",
        "SET ROLE trigger_acl_creator",
        "CREATE TRIGGER trigger_acl_allowed BEFORE INSERT ON trigger_acl_items FOR EACH ROW EXECUTE FUNCTION trigger_acl_passthrough()",
    ] {
        execute(&engine, sql);
    }
}
