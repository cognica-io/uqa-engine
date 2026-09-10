//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn text_pairs(engine: &Engine, sql: &str) -> Vec<String> {
    exec(engine, sql)
        .rows
        .into_iter()
        .map(|row| match row.get("entry") {
            Some(Value::Str(value)) => value.clone(),
            other => panic!("expected text column `entry`, got {other:?}"),
        })
        .collect()
}

#[test]
fn rule_conditions_execute_correlated_and_uncorrelated_subqueries() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_subquery_items(id INTEGER, payload INTEGER);
         CREATE TABLE condition_subquery_lookup(id INTEGER);
         CREATE TABLE condition_subquery_log(kind TEXT, id INTEGER);
         INSERT INTO condition_subquery_lookup VALUES (2)",
    );
    for rule in [
        "CREATE RULE a_exists_constant AS ON INSERT TO condition_subquery_items WHERE EXISTS (SELECT 1) DO ALSO INSERT INTO condition_subquery_log VALUES ('constant', NEW.id)",
        "CREATE RULE b_exists_correlated AS ON INSERT TO condition_subquery_items WHERE EXISTS (SELECT 1 WHERE NEW.id > 0) DO ALSO INSERT INTO condition_subquery_log VALUES ('correlated', NEW.id)",
        "CREATE RULE c_scalar_correlated AS ON INSERT TO condition_subquery_items WHERE (SELECT NEW.id > 1) DO ALSO INSERT INTO condition_subquery_log VALUES ('scalar', NEW.id)",
        "CREATE RULE d_in_subquery AS ON INSERT TO condition_subquery_items WHERE NEW.id IN (SELECT 2) DO ALSO INSERT INTO condition_subquery_log VALUES ('in', NEW.id)",
        "CREATE RULE e_external_relation AS ON INSERT TO condition_subquery_items WHERE EXISTS (SELECT 1 FROM condition_subquery_lookup AS lookup WHERE lookup.id = NEW.id) DO ALSO INSERT INTO condition_subquery_log VALUES ('external', NEW.id)",
        "CREATE RULE f_local_unqualified AS ON INSERT TO condition_subquery_items WHERE EXISTS (SELECT 1 FROM condition_subquery_lookup WHERE id = 2) DO ALSO INSERT INTO condition_subquery_log VALUES ('local', NEW.id)",
    ] {
        exec(&engine, rule);
    }

    exec(
        &engine,
        "INSERT INTO condition_subquery_items VALUES (0, 10), (2, 20)",
    );
    assert_eq!(
        text_pairs(
            &engine,
            "SELECT kind || ':' || id AS entry FROM condition_subquery_log ORDER BY id, kind",
        ),
        [
            "constant:0",
            "local:0",
            "constant:2",
            "correlated:2",
            "external:2",
            "in:2",
            "local:2",
            "scalar:2",
        ]
    );
}

#[test]
fn insert_rule_condition_subqueries_observe_action_time_state() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_timing_also(id INTEGER);
         CREATE TABLE condition_timing_also_log(id INTEGER);
         CREATE RULE condition_timing_also_rule AS ON INSERT TO condition_timing_also
           WHERE NOT EXISTS (SELECT 1 FROM condition_timing_also AS seen WHERE seen.id = NEW.id)
           DO ALSO INSERT INTO condition_timing_also_log VALUES (NEW.id)",
    );
    exec(&engine, "INSERT INTO condition_timing_also VALUES (1)");
    assert!(exec(&engine, "SELECT * FROM condition_timing_also_log")
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE TABLE condition_timing_instead(id INTEGER);
         CREATE TABLE condition_timing_instead_log(id INTEGER);
         CREATE RULE condition_timing_instead_rule AS ON INSERT TO condition_timing_instead
           WHERE EXISTS (SELECT 1 FROM condition_timing_instead AS seen WHERE seen.id = NEW.id)
           DO INSTEAD INSERT INTO condition_timing_instead_log VALUES (NEW.id)",
    );
    exec(&engine, "INSERT INTO condition_timing_instead VALUES (1)");
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_timing_instead")
            .rows
            .len(),
        1
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_timing_instead_log")
            .rows
            .len(),
        1
    );
}

#[test]
fn scalar_rule_condition_subquery_failure_is_statement_atomic() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_cardinality_items(id INTEGER);
         CREATE TABLE condition_cardinality_log(id INTEGER);
         CREATE RULE condition_cardinality_rule AS ON INSERT TO condition_cardinality_items
           WHERE (SELECT accepted FROM (VALUES (true), (false)) AS candidates(accepted))
           DO ALSO INSERT INTO condition_cardinality_log VALUES (NEW.id)",
    );
    let error = engine
        .sql("INSERT INTO condition_cardinality_items VALUES (1)", &[])
        .expect_err("a scalar subquery must return at most one row");
    assert_eq!(error.sqlstate(), Some("21000"), "{error}");
    assert!(exec(&engine, "SELECT * FROM condition_cardinality_items")
        .rows
        .is_empty());
    assert!(exec(&engine, "SELECT * FROM condition_cardinality_log")
        .rows
        .is_empty());
}

#[test]
fn rule_condition_subqueries_bind_names_and_validate_types_at_creation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_binding_items(id INTEGER);
         CREATE TABLE condition_binding_log(id INTEGER);
         CREATE TABLE condition_binding_lookup(id INTEGER);
         INSERT INTO condition_binding_lookup VALUES (1)",
    );
    exec(
        &engine,
        "CREATE RULE condition_shadow_new AS ON INSERT TO condition_binding_items
           WHERE EXISTS (SELECT 1 FROM condition_binding_lookup AS new WHERE new.id = 1)
           DO ALSO INSERT INTO condition_binding_log VALUES (NEW.id)",
    );
    exec(&engine, "INSERT INTO condition_binding_items VALUES (7)");
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_binding_log").rows[0].get("id"),
        Some(&Value::Int(7))
    );

    let wrong_type = engine
        .sql(
            "CREATE RULE condition_wrong_type AS ON INSERT TO condition_binding_items WHERE (SELECT 1) DO NOTHING",
            &[],
        )
        .expect_err("a rule qualification must be boolean");
    assert_eq!(wrong_type.sqlstate(), Some("42804"));
    let missing_relation = engine
        .sql(
            "CREATE RULE condition_missing_relation AS ON INSERT TO condition_binding_items WHERE EXISTS (SELECT 1 FROM condition_binding_missing) DO NOTHING",
            &[],
        )
        .expect_err("a stored condition relation must exist at creation");
    assert_eq!(missing_relation.sqlstate(), Some("42P01"));
    let invalid_old = engine
        .sql(
            "CREATE RULE condition_invalid_old AS ON INSERT TO condition_binding_items WHERE EXISTS (SELECT 1 WHERE OLD.id = 1) DO NOTHING",
            &[],
        )
        .expect_err("an INSERT qualification cannot reference OLD");
    assert_eq!(invalid_old.sqlstate(), Some("42P01"));
}

#[test]
fn rule_condition_subquery_relations_are_bound_across_search_path_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("condition-subquery.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA condition_bound_first;
             CREATE SCHEMA condition_bound_second;
             CREATE TABLE condition_bound_first.lookup(id INTEGER);
             CREATE TABLE condition_bound_second.lookup(id INTEGER);
             CREATE TABLE condition_bound_first.items(id INTEGER);
             CREATE TABLE condition_bound_first.log(id INTEGER);
             INSERT INTO condition_bound_first.lookup VALUES (1);
             INSERT INTO condition_bound_second.lookup VALUES (2);
             SET search_path = condition_bound_first, public;
             CREATE RULE condition_bound_rule AS ON INSERT TO items
               WHERE EXISTS (SELECT 1 FROM lookup WHERE id = NEW.id)
               DO ALSO INSERT INTO condition_bound_first.log VALUES (NEW.id);
             SET search_path = condition_bound_second, public",
        );
        exec(
            &engine,
            "INSERT INTO condition_bound_first.items VALUES (1), (2)",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM condition_bound_first.log")
                .rows
                .iter()
                .map(|row| row.get("id"))
                .collect::<Vec<_>>(),
            [Some(&Value::Int(1))]
        );
    }
    let engine = Engine::open(&path).expect("a bound rule condition plan must restore");
    exec(
        &engine,
        "SET search_path = condition_bound_second, public;
         INSERT INTO condition_bound_first.items VALUES (3), (2)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT id FROM condition_bound_first.log ORDER BY id",
        )
        .rows
        .iter()
        .map(|row| row.get("id"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(1))]
    );
}

#[test]
fn rule_condition_subquery_routines_are_bound_across_search_path_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("condition-subquery-routine.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA condition_routine_first;
             CREATE SCHEMA condition_routine_second;
             CREATE FUNCTION condition_routine_first.accepted(value INTEGER) RETURNS BOOLEAN LANGUAGE SQL IMMUTABLE AS 'SELECT true';
             CREATE FUNCTION condition_routine_second.accepted(value INTEGER) RETURNS BOOLEAN LANGUAGE SQL IMMUTABLE AS 'SELECT false';
             CREATE TABLE condition_routine_first.items(id INTEGER);
             CREATE TABLE condition_routine_first.log(id INTEGER);
             SET search_path = condition_routine_first, public;
             CREATE RULE condition_routine_rule AS ON INSERT TO items
               WHERE EXISTS (SELECT 1 WHERE accepted(NEW.id))
               DO ALSO INSERT INTO condition_routine_first.log VALUES (NEW.id);
             SET search_path = condition_routine_second, public;
             INSERT INTO condition_routine_first.items VALUES (1)",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM condition_routine_first.log")
                .rows
                .len(),
            1
        );
    }
    let engine = Engine::open(&path).expect("a bound rule condition routine must restore");
    exec(
        &engine,
        "SET search_path = condition_routine_second, public;
         INSERT INTO condition_routine_first.items VALUES (2)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT id FROM condition_routine_first.log ORDER BY id",
        )
        .rows
        .iter()
        .map(|row| row.get("id"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(2))]
    );
}

#[test]
fn rule_condition_subquery_event_columns_follow_rename_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("condition-subquery-rename.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE condition_rename_items(id INTEGER);
             CREATE TABLE condition_rename_lookup(id INTEGER);
             CREATE TABLE condition_rename_log(id INTEGER);
             INSERT INTO condition_rename_lookup VALUES (1);
             CREATE RULE condition_rename_rule AS ON INSERT TO condition_rename_items
               WHERE EXISTS (SELECT 1 FROM condition_rename_lookup WHERE id = 1 AND NEW.id > 0)
               DO ALSO INSERT INTO condition_rename_log VALUES (NEW.id);
             ALTER TABLE condition_rename_items RENAME COLUMN id TO item_id;
             INSERT INTO condition_rename_items(item_id) VALUES (2)",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM condition_rename_log").rows[0].get("id"),
            Some(&Value::Int(2))
        );
        let definition = exec(
            &engine,
            "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'condition_rename_rule'",
        );
        let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
            panic!("expected rule definition text");
        };
        assert!(definition.contains("new.item_id"), "{definition}");
        assert!(definition.contains("id = 1"), "{definition}");
        assert!(!definition.contains("item_id = 1"), "{definition}");
    }
    let engine = Engine::open(&path).expect("a renamed rule condition plan must restore");
    exec(
        &engine,
        "INSERT INTO condition_rename_items(item_id) VALUES (3)",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_rename_log ORDER BY id")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(2)), Some(&Value::Int(3))]
    );
    let dependent = engine
        .sql(
            "ALTER TABLE condition_rename_items DROP COLUMN item_id",
            &[],
        )
        .expect_err("the stored condition plan must retain its event-column dependency");
    assert_eq!(dependent.sqlstate(), Some("2BP01"), "{dependent}");
}

#[test]
fn rule_condition_subquery_relations_use_the_rule_owner_for_privileges() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE condition_subquery_owner",
        "CREATE ROLE condition_subquery_caller",
        "GRANT CREATE ON DATABASE uqa TO condition_subquery_owner",
        "SET ROLE condition_subquery_owner",
        "CREATE SCHEMA condition_subquery_security",
        "GRANT USAGE, CREATE ON SCHEMA condition_subquery_security TO condition_subquery_caller",
        "CREATE TABLE condition_subquery_security.owner_lookup(id INTEGER)",
        "CREATE TABLE condition_subquery_security.owner_event(id INTEGER)",
        "CREATE TABLE condition_subquery_security.caller_event(id INTEGER)",
        "CREATE TABLE condition_subquery_security.owner_function_event(id INTEGER)",
        "CREATE TABLE condition_subquery_security.caller_function_event(id INTEGER)",
        "CREATE TABLE condition_subquery_security.log(kind TEXT, id INTEGER)",
        "CREATE FUNCTION condition_subquery_security.owner_only() RETURNS BOOLEAN LANGUAGE SQL AS 'SELECT true'",
        "CREATE FUNCTION condition_subquery_security.caller_only() RETURNS BOOLEAN LANGUAGE SQL AS 'SELECT true'",
        "REVOKE ALL ON FUNCTION condition_subquery_security.owner_only(), condition_subquery_security.caller_only() FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION condition_subquery_security.caller_only() TO condition_subquery_caller",
        "INSERT INTO condition_subquery_security.owner_lookup VALUES (1)",
        "REVOKE ALL ON condition_subquery_security.owner_lookup FROM PUBLIC",
        "GRANT INSERT ON condition_subquery_security.owner_event, condition_subquery_security.caller_event, condition_subquery_security.owner_function_event, condition_subquery_security.caller_function_event TO condition_subquery_caller",
        "RESET ROLE",
        "SET ROLE condition_subquery_caller",
        "CREATE TABLE condition_subquery_security.caller_lookup(id INTEGER)",
        "INSERT INTO condition_subquery_security.caller_lookup VALUES (1)",
        "REVOKE ALL ON condition_subquery_security.caller_lookup FROM PUBLIC",
        "RESET ROLE",
        "SET ROLE condition_subquery_owner",
        "CREATE RULE condition_subquery_owner_rule AS ON INSERT TO condition_subquery_security.owner_event WHERE EXISTS (SELECT 1 FROM condition_subquery_security.owner_lookup WHERE id = NEW.id) DO ALSO INSERT INTO condition_subquery_security.log VALUES ('owner', NEW.id)",
        "CREATE RULE condition_subquery_caller_rule AS ON INSERT TO condition_subquery_security.caller_event WHERE EXISTS (SELECT 1 FROM condition_subquery_security.caller_lookup WHERE id = NEW.id) DO ALSO INSERT INTO condition_subquery_security.log VALUES ('caller', NEW.id)",
        "CREATE RULE condition_subquery_owner_function_rule AS ON INSERT TO condition_subquery_security.owner_function_event WHERE EXISTS (SELECT 1 WHERE condition_subquery_security.owner_only()) DO ALSO INSERT INTO condition_subquery_security.log VALUES ('owner-function', NEW.id)",
        "CREATE RULE condition_subquery_caller_function_rule AS ON INSERT TO condition_subquery_security.caller_function_event WHERE EXISTS (SELECT 1 WHERE condition_subquery_security.caller_only()) DO ALSO INSERT INTO condition_subquery_security.log VALUES ('caller-function', NEW.id)",
        "RESET ROLE",
        "SET ROLE condition_subquery_caller",
    ] {
        exec(&engine, sql);
    }

    exec(
        &engine,
        "INSERT INTO condition_subquery_security.owner_event VALUES (1)",
    );
    let denied = engine
        .sql(
            "INSERT INTO condition_subquery_security.caller_event VALUES (1)",
            &[],
        )
        .expect_err("the invoker's SELECT privilege must not replace the rule owner's privilege");
    assert_eq!(denied.sqlstate(), Some("42501"), "{denied}");
    let function_denied = engine
        .sql(
            "INSERT INTO condition_subquery_security.owner_function_event VALUES (1)",
            &[],
        )
        .expect_err("condition routines must retain the invoker as their privilege subject");
    assert_eq!(
        function_denied.sqlstate(),
        Some("42501"),
        "{function_denied}"
    );
    exec(
        &engine,
        "INSERT INTO condition_subquery_security.caller_function_event VALUES (1)",
    );
    exec(&engine, "RESET ROLE");
    assert_eq!(
        text_pairs(
            &engine,
            "SELECT kind || ':' || id AS entry FROM condition_subquery_security.log ORDER BY kind",
        ),
        ["caller-function:1", "owner:1"]
    );
    assert!(exec(
        &engine,
        "SELECT * FROM condition_subquery_security.caller_event",
    )
    .rows
    .is_empty());
    assert!(exec(
        &engine,
        "SELECT * FROM condition_subquery_security.owner_function_event",
    )
    .rows
    .is_empty());
}

#[test]
fn update_and_delete_rule_condition_subqueries_bind_old_and_new_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_mutation_items(id INTEGER PRIMARY KEY, value INTEGER);
         CREATE TABLE condition_mutation_lookup(id INTEGER);
         CREATE TABLE condition_mutation_log(entry TEXT);
         INSERT INTO condition_mutation_items VALUES (1, 10), (2, 20);
         INSERT INTO condition_mutation_lookup VALUES (2);
         CREATE RULE condition_mutation_update AS ON UPDATE TO condition_mutation_items
           WHERE EXISTS (WITH delta AS (SELECT NEW.value - OLD.value AS amount) SELECT 1 FROM delta, condition_mutation_lookup AS lookup WHERE amount > 0 AND lookup.id = NEW.id)
           DO ALSO INSERT INTO condition_mutation_log VALUES ('update:' || OLD.value || ':' || NEW.value);
         CREATE RULE condition_mutation_delete AS ON DELETE TO condition_mutation_items
           WHERE OLD.id IN (SELECT id FROM condition_mutation_lookup)
           DO INSTEAD INSERT INTO condition_mutation_log VALUES ('retain:' || OLD.id)",
    );
    exec(
        &engine,
        "UPDATE condition_mutation_items SET value = value + 1",
    );
    exec(&engine, "DELETE FROM condition_mutation_items");
    assert_eq!(
        text_pairs(
            &engine,
            "SELECT entry FROM condition_mutation_log ORDER BY entry",
        ),
        ["retain:2", "update:20:21"]
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_mutation_items")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(2))]
    );

    let ambiguous = engine
        .sql(
            "CREATE RULE condition_mutation_ambiguous AS ON UPDATE TO condition_mutation_items WHERE EXISTS (SELECT 1 WHERE value > 0) DO NOTHING",
            &[],
        )
        .expect_err("an unqualified UPDATE event column has both OLD and NEW owners");
    assert_eq!(ambiguous.sqlstate(), Some("42702"));
}
