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

fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn relation_schema_fixture() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE relation_schema_caller",
        "CREATE ROLE relation_schema_group",
        "CREATE ROLE relation_schema_member INHERIT",
        "GRANT relation_schema_group TO relation_schema_member",
        "CREATE SCHEMA relation_schema_hidden",
        "CREATE SCHEMA relation_schema_visible",
        "REVOKE ALL ON SCHEMA relation_schema_hidden, relation_schema_visible FROM PUBLIC",
        "CREATE TABLE relation_schema_hidden.only_table(value integer)",
        "INSERT INTO relation_schema_hidden.only_table VALUES (1)",
        "CREATE TABLE relation_schema_visible.only_table(value integer)",
        "INSERT INTO relation_schema_visible.only_table VALUES (2)",
        "CREATE TABLE relation_schema_hidden.only_hidden(value integer)",
        "INSERT INTO relation_schema_hidden.only_hidden VALUES (7)",
        "CREATE TABLE relation_schema_hidden.event_items(id integer)",
        "CREATE TABLE relation_schema_visible.event_items(id integer)",
        "CREATE TABLE relation_schema_hidden.event_log(id integer)",
        "CREATE TABLE relation_schema_hidden.event_updates(id integer)",
        "INSERT INTO relation_schema_hidden.event_updates VALUES (0)",
        "CREATE TABLE relation_schema_hidden.event_deletes(id integer)",
        "INSERT INTO relation_schema_hidden.event_deletes VALUES (41)",
        "CREATE TABLE relation_schema_hidden.parent_rows(id integer, hidden_value integer)",
        "CREATE TABLE relation_schema_visible.parent_rows(id integer, visible_value integer)",
        "CREATE TABLE relation_schema_hidden.reference_rows(id integer PRIMARY KEY)",
        "CREATE TABLE relation_schema_visible.reference_rows(id integer PRIMARY KEY)",
        "INSERT INTO relation_schema_hidden.reference_rows VALUES (2)",
        "INSERT INTO relation_schema_visible.reference_rows VALUES (1)",
        "CREATE TABLE relation_schema_hidden.partitioned_rows(id integer) PARTITION BY RANGE(id)",
        "CREATE TABLE relation_schema_visible.partitioned_rows(id integer) PARTITION BY RANGE(id)",
        "CREATE TABLE relation_schema_hidden.attach_rows(id integer)",
        "CREATE TABLE relation_schema_visible.alter_rows(id integer)",
        "CREATE FUNCTION relation_schema_visible.event_trigger() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'",
        "CREATE FUNCTION relation_schema_hidden.bound_event_trigger() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'",
        "CREATE SERVER relation_schema_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE relation_schema_hidden.hidden_foreign(value integer) SERVER relation_schema_memory",
        "CREATE VIEW relation_schema_visible.bound_view AS SELECT value FROM relation_schema_hidden.only_hidden",
        "CREATE MATERIALIZED VIEW relation_schema_visible.bound_matview AS SELECT value FROM relation_schema_hidden.only_hidden",
        "CREATE FUNCTION relation_schema_visible.bound_function() RETURNS integer LANGUAGE SQL RETURN (SELECT value FROM relation_schema_hidden.only_hidden)",
        "CREATE FUNCTION relation_schema_visible.dynamic_invoker() RETURNS integer LANGUAGE SQL SECURITY INVOKER AS 'SELECT value FROM relation_schema_hidden.only_hidden'",
        "CREATE FUNCTION relation_schema_visible.dynamic_definer() RETURNS integer LANGUAGE SQL SECURITY DEFINER AS 'SELECT value FROM relation_schema_hidden.only_hidden'",
        "GRANT USAGE, CREATE ON SCHEMA relation_schema_visible TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "SET search_path TO relation_schema_hidden, relation_schema_visible, pg_catalog",
    ] {
        execute(&engine, sql);
    }
    engine
}

#[test]
fn pg18_relation_lookup_filters_the_effective_search_path_and_checks_qualified_names() {
    let engine = relation_schema_fixture();

    assert_eq!(
        scalar(&engine, "SELECT value FROM only_table"),
        Value::Int(2)
    );
    assert_eq!(failure(&engine, "SELECT value FROM only_hidden").0, "42P01");
    assert_eq!(
        failure(&engine, "SELECT value FROM relation_schema_missing.absent").0,
        "42P01"
    );
    for sql in [
        "SELECT value FROM relation_schema_hidden.only_hidden",
        "SELECT value FROM relation_schema_hidden.absent",
        "SELECT missing_column FROM relation_schema_hidden.only_hidden",
        "SELECT value FROM relation_schema_hidden.only_hidden WHERE missing_column = 1",
        "SELECT value FROM relation_schema_hidden.hidden_foreign",
        "SELECT 'relation_schema_hidden.only_hidden'::regclass",
        "SELECT to_regclass('relation_schema_hidden.only_hidden')",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }
}

#[test]
fn pg18_mutation_and_relation_ddl_resolve_the_namespace_before_object_details() {
    let engine = relation_schema_fixture();

    for sql in [
        "INSERT INTO relation_schema_hidden.only_hidden(missing_column) VALUES (1)",
        "UPDATE relation_schema_hidden.only_hidden SET missing_column = 1",
        "DELETE FROM relation_schema_hidden.only_hidden WHERE missing_column = 1",
        "TRUNCATE relation_schema_hidden.only_hidden",
        "ALTER TABLE relation_schema_hidden.only_hidden DROP COLUMN missing_column",
        "DROP TABLE relation_schema_hidden.only_hidden",
        "CREATE VIEW relation_schema_visible.denied_view AS SELECT value FROM relation_schema_hidden.only_hidden",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }
}

#[test]
fn pg18_trigger_and_rule_names_resolve_through_the_effective_namespace_once() {
    let engine = relation_schema_fixture();

    for sql in [
        "CREATE TRIGGER denied_trigger BEFORE INSERT ON relation_schema_hidden.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "CREATE TRIGGER denied_missing_trigger BEFORE INSERT ON relation_schema_hidden.missing_items FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "DROP TRIGGER missing_trigger ON relation_schema_hidden.event_items",
        "CREATE RULE denied_rule AS ON INSERT TO relation_schema_hidden.event_items DO ALSO NOTHING",
        "CREATE RULE denied_missing_rule AS ON INSERT TO relation_schema_hidden.missing_items DO ALSO NOTHING",
        "DROP RULE missing_rule ON relation_schema_hidden.event_items",
        "CREATE RULE denied_action_rule AS ON INSERT TO relation_schema_visible.event_items DO ALSO INSERT INTO relation_schema_hidden.event_log VALUES (NEW.id)",
        "CREATE CONSTRAINT TRIGGER denied_reference_trigger AFTER INSERT ON relation_schema_visible.event_items FROM relation_schema_hidden.event_items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "CREATE TRIGGER denied_function_trigger BEFORE INSERT ON relation_schema_visible.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_hidden.bound_event_trigger()",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }

    for sql in [
        "CREATE TRIGGER visible_trigger BEFORE INSERT ON event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "DROP TRIGGER visible_trigger ON relation_schema_visible.event_items",
        "CREATE TRIGGER visible_drop_trigger BEFORE INSERT ON relation_schema_visible.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "DROP TRIGGER visible_drop_trigger ON event_items",
        "CREATE RULE visible_rule AS ON INSERT TO event_items DO ALSO NOTHING",
        "DROP RULE visible_rule ON relation_schema_visible.event_items",
        "CREATE RULE visible_drop_rule AS ON INSERT TO relation_schema_visible.event_items DO ALSO NOTHING",
        "DROP RULE visible_drop_rule ON event_items",
    ] {
        execute(&engine, sql);
    }

    for sql in [
        "CREATE TRIGGER missing_schema_trigger BEFORE INSERT ON relation_schema_missing.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "DROP TRIGGER missing_trigger ON relation_schema_missing.event_items",
        "CREATE RULE missing_schema_rule AS ON INSERT TO relation_schema_missing.event_items DO ALSO NOTHING",
        "DROP RULE missing_rule ON relation_schema_missing.event_items",
        "CREATE CONSTRAINT TRIGGER missing_reference_trigger AFTER INSERT ON relation_schema_visible.event_items FROM relation_schema_missing.event_items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION relation_schema_visible.event_trigger()",
        "CREATE TRIGGER missing_function_trigger BEFORE INSERT ON relation_schema_visible.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_missing.event_trigger()",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "3F000", "{sql}: {message}");
        assert_eq!(
            message, "schema \"relation_schema_missing\" does not exist",
            "{sql}"
        );
    }
    let action_sql = "CREATE RULE missing_action_rule AS ON INSERT TO relation_schema_visible.event_items DO ALSO INSERT INTO relation_schema_missing.event_log VALUES (NEW.id)";
    let (state, message) = failure(&engine, action_sql);
    assert_eq!(state, "42P01", "{action_sql}: {message}");
    assert_eq!(
        message, "relation \"relation_schema_missing.event_log\" does not exist",
        "{action_sql}"
    );

    engine.take_sql_notices();
    for sql in [
        "DROP TRIGGER IF EXISTS missing_trigger ON relation_schema_missing.event_items",
        "DROP RULE IF EXISTS missing_rule ON relation_schema_missing.event_items",
        "DROP TRIGGER IF EXISTS missing_trigger ON relation_schema_visible.missing_items",
        "DROP RULE IF EXISTS missing_rule ON relation_schema_visible.missing_items",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        engine.take_sql_notices(),
        [
            (
                "NOTICE".into(),
                "schema \"relation_schema_missing\" does not exist, skipping".into(),
            ),
            (
                "NOTICE".into(),
                "schema \"relation_schema_missing\" does not exist, skipping".into(),
            ),
            (
                "NOTICE".into(),
                "relation \"relation_schema_visible.missing_items\" does not exist, skipping"
                    .into(),
            ),
            (
                "NOTICE".into(),
                "relation \"relation_schema_visible.missing_items\" does not exist, skipping"
                    .into(),
            ),
        ]
    );
}

#[test]
fn pg18_stored_rule_actions_keep_their_authorized_relation_identity() {
    let engine = relation_schema_fixture();
    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "CREATE RULE bound_insert_action AS ON INSERT TO relation_schema_visible.event_items DO ALSO INSERT INTO relation_schema_hidden.event_log VALUES (NEW.id)",
        "CREATE RULE bound_update_action AS ON INSERT TO relation_schema_visible.event_items DO ALSO UPDATE relation_schema_hidden.event_updates SET id = NEW.id",
        "CREATE RULE bound_delete_action AS ON INSERT TO relation_schema_visible.event_items DO ALSO DELETE FROM relation_schema_hidden.event_deletes WHERE id = NEW.id",
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "INSERT INTO relation_schema_visible.event_items VALUES (41)",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM relation_schema_hidden.event_log WHERE id = 41"
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT id FROM relation_schema_hidden.event_updates"
        ),
        Value::Int(41)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM relation_schema_hidden.event_deletes"
        ),
        Value::Int(0)
    );
}

#[test]
fn legacy_stored_rule_actions_restore_as_bound_relation_identities() {
    use uqa_storage::{Catalog, ManagedConnection};

    fn remove_target_binding_marker(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                usize::from(fields.remove("target_relation_bound").is_some())
                    + fields
                        .values_mut()
                        .map(remove_target_binding_marker)
                        .sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                values.iter_mut().map(remove_target_binding_marker).sum()
            }
            _ => 0,
        }
    }

    let directory = tempfile::TempDir::new().unwrap();
    let database = directory.path().join("bound-rule-action.db");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE ROLE bound_rule_caller",
            "CREATE SCHEMA bound_rule_hidden",
            "CREATE SCHEMA bound_rule_visible",
            "REVOKE ALL ON SCHEMA bound_rule_hidden, bound_rule_visible FROM PUBLIC",
            "GRANT USAGE, CREATE ON SCHEMA bound_rule_hidden, bound_rule_visible TO bound_rule_caller",
            "SET ROLE bound_rule_caller",
            "CREATE TABLE bound_rule_visible.event_items(id integer)",
            "CREATE TABLE bound_rule_hidden.event_log(id integer)",
            "CREATE RULE bound_rule_action AS ON INSERT TO bound_rule_visible.event_items DO ALSO INSERT INTO bound_rule_hidden.event_log VALUES (NEW.id)",
            "RESET ROLE",
            "REVOKE USAGE ON SCHEMA bound_rule_hidden FROM bound_rule_caller",
        ] {
            execute(&engine, sql);
        }
    }
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(remove_target_binding_marker(&mut metadata), 1);
        catalog
            .set_metadata("sql_rules_json", &serde_json::to_string(&metadata).unwrap())
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    execute(&engine, "SET ROLE bound_rule_caller");
    execute(
        &engine,
        "INSERT INTO bound_rule_visible.event_items VALUES (91)",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM bound_rule_hidden.event_log WHERE id = 91"
        ),
        Value::Int(1)
    );
}

#[test]
fn pg18_hierarchy_and_foreign_key_references_use_the_effective_namespace() {
    let engine = relation_schema_fixture();

    for sql in [
        "CREATE TABLE relation_schema_visible.denied_inherited(local_value integer) INHERITS (relation_schema_hidden.parent_rows)",
        "CREATE TABLE relation_schema_visible.denied_partition PARTITION OF relation_schema_hidden.partitioned_rows FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE relation_schema_visible.denied_column_fk(id integer REFERENCES relation_schema_hidden.reference_rows(id))",
        "CREATE TABLE relation_schema_visible.denied_table_fk(id integer, FOREIGN KEY (id) REFERENCES relation_schema_hidden.reference_rows(id))",
        "ALTER TABLE relation_schema_visible.alter_rows ADD CONSTRAINT denied_alter_fk FOREIGN KEY (id) REFERENCES relation_schema_hidden.reference_rows(id)",
        "ALTER TABLE relation_schema_visible.alter_rows INHERIT relation_schema_hidden.parent_rows",
        "ALTER TABLE relation_schema_visible.partitioned_rows ATTACH PARTITION relation_schema_hidden.attach_rows FOR VALUES FROM (10) TO (20)",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(
            message, "permission denied for schema relation_schema_hidden",
            "{sql}"
        );
    }

    for sql in [
        "CREATE TABLE relation_schema_visible.missing_inherited(local_value integer) INHERITS (relation_schema_missing.parent_rows)",
        "CREATE TABLE relation_schema_visible.missing_partition PARTITION OF relation_schema_missing.partitioned_rows FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE relation_schema_visible.missing_column_fk(id integer REFERENCES relation_schema_missing.reference_rows(id))",
        "CREATE TABLE relation_schema_visible.missing_table_fk(id integer, FOREIGN KEY (id) REFERENCES relation_schema_missing.reference_rows(id))",
        "ALTER TABLE relation_schema_visible.alter_rows ADD CONSTRAINT missing_alter_fk FOREIGN KEY (id) REFERENCES relation_schema_missing.reference_rows(id)",
        "ALTER TABLE relation_schema_visible.alter_rows INHERIT relation_schema_missing.parent_rows",
        "ALTER TABLE relation_schema_visible.partitioned_rows ATTACH PARTITION relation_schema_missing.attach_rows FOR VALUES FROM (10) TO (20)",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "3F000", "{sql}: {message}");
        assert_eq!(
            message, "schema \"relation_schema_missing\" does not exist",
            "{sql}"
        );
    }

    for sql in [
        "CREATE TABLE relation_schema_visible.inherited_rows(local_value integer) INHERITS (parent_rows)",
        "CREATE TABLE relation_schema_visible.partition_rows PARTITION OF partitioned_rows FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE relation_schema_visible.foreign_key_rows(id integer REFERENCES reference_rows(id))",
        "INSERT INTO relation_schema_visible.foreign_key_rows VALUES (1)",
    ] {
        execute(&engine, sql);
    }
    for child in ["inherited_rows", "partition_rows"] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT parent_namespace.nspname FROM pg_catalog.pg_inherits inheritance JOIN pg_catalog.pg_class child ON child.oid = inheritance.inhrelid JOIN pg_catalog.pg_namespace child_namespace ON child_namespace.oid = child.relnamespace JOIN pg_catalog.pg_class parent ON parent.oid = inheritance.inhparent JOIN pg_catalog.pg_namespace parent_namespace ON parent_namespace.oid = parent.relnamespace WHERE child_namespace.nspname = 'relation_schema_visible' AND child.relname = '{child}'"
                )
            ),
            Value::Str("relation_schema_visible".into()),
            "{child}"
        );
    }
    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "CREATE TABLE relation_schema_visible.bound_foreign_key_rows(id integer REFERENCES relation_schema_hidden.reference_rows(id))",
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
    ] {
        execute(&engine, sql);
    }
    let (state, message) = failure(
        &engine,
        "INSERT INTO relation_schema_visible.bound_foreign_key_rows VALUES (2)",
    );
    assert_eq!(state, "42501");
    assert_eq!(
        message,
        "permission denied for schema relation_schema_hidden"
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM relation_schema_visible.bound_foreign_key_rows"
        ),
        Value::Int(0)
    );
}

#[test]
fn pg18_stored_relation_identities_do_not_repeat_namespace_name_checks() {
    let engine = relation_schema_fixture();

    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_visible.bound_view"
        ),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_visible.bound_matview"
        ),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relation_schema_visible.bound_function() AS value"
        ),
        Value::Int(7)
    );
    assert_eq!(
        failure(
            &engine,
            "SELECT relation_schema_visible.dynamic_invoker() AS value"
        )
        .0,
        "42501"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relation_schema_visible.dynamic_definer() AS value"
        ),
        Value::Int(7)
    );

    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "CREATE TRIGGER bound_function_trigger BEFORE INSERT ON relation_schema_visible.event_items FOR EACH ROW EXECUTE FUNCTION relation_schema_hidden.bound_event_trigger()",
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "INSERT INTO relation_schema_visible.event_items VALUES (73)",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM relation_schema_visible.event_items WHERE id = 73"
        ),
        Value::Int(1)
    );
}

#[test]
fn pg18_prepared_and_inherited_relation_namespace_access_tracks_live_acl_state() {
    let engine = relation_schema_fixture();
    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "PREPARE relation_schema_prepared AS SELECT value FROM relation_schema_hidden.only_hidden",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "EXECUTE relation_schema_prepared"),
        Value::Int(7)
    );
    for sql in [
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        failure(&engine, "EXECUTE relation_schema_prepared").0,
        "42501"
    );

    for sql in [
        "RESET ROLE",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_caller",
        "SET ROLE relation_schema_caller",
        "BEGIN",
        "DECLARE relation_schema_cursor CURSOR FOR SELECT value FROM relation_schema_hidden.only_hidden",
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "SET ROLE relation_schema_caller",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(&engine, "FETCH relation_schema_cursor"),
        Value::Int(7)
    );
    execute(&engine, "ROLLBACK");

    for sql in [
        "RESET ROLE",
        "REVOKE USAGE ON SCHEMA relation_schema_hidden FROM relation_schema_caller",
        "GRANT USAGE ON SCHEMA relation_schema_hidden TO relation_schema_group",
        "SET ROLE relation_schema_member",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT value FROM relation_schema_hidden.only_hidden"
        ),
        Value::Int(7)
    );
}
