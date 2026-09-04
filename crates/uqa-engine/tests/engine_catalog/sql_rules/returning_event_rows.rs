//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rule_action_returning_uses_postgresql_event_and_action_row_namespaces() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_insert_event(e INTEGER, x TEXT);
         CREATE TABLE returning_insert_target(i INTEGER, y TEXT);
         CREATE RULE returning_insert_provider AS ON INSERT TO returning_insert_event DO INSTEAD
           INSERT INTO returning_insert_target VALUES (NEW.e + 100, NEW.x || '-action')
           RETURNING NEW.*",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO returning_insert_event VALUES (1, 'event')
         RETURNING e, x, old.x, new.x",
    );
    assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(101)));
    assert_eq!(
        inserted.value_at(0, 1),
        Some(&Value::Str("event-action".into()))
    );
    assert_eq!(inserted.value_at(0, 2), Some(&Value::Null));
    assert_eq!(
        inserted.value_at(0, 3),
        Some(&Value::Str("event-action".into()))
    );

    exec(
        &engine,
        "CREATE TABLE returning_update_event(e INTEGER PRIMARY KEY, x TEXT);
         CREATE TABLE returning_update_target(i INTEGER PRIMARY KEY, y TEXT);
         INSERT INTO returning_update_event VALUES (2, 'event-old');
         INSERT INTO returning_update_target VALUES (2, 'action-old');
         CREATE RULE returning_update_provider AS ON UPDATE TO returning_update_event DO INSTEAD
           UPDATE returning_update_target AS trgt
           SET y = NEW.x || '-action' WHERE trgt.i = OLD.e
           RETURNING NEW.*",
    );
    let updated = exec(
        &engine,
        "UPDATE returning_update_event SET x = 'event-new' WHERE e = 2
         RETURNING e, x, old.x, new.x",
    );
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(2)));
    for position in 1..=3 {
        assert_eq!(
            updated.value_at(0, position),
            Some(&Value::Str("event-new".into()))
        );
    }
    assert_eq!(
        exec(&engine, "SELECT x FROM returning_update_event").value_at(0, 0),
        Some(&Value::Str("event-old".into()))
    );
    assert_eq!(
        exec(&engine, "SELECT y FROM returning_update_target").value_at(0, 0),
        Some(&Value::Str("event-new-action".into()))
    );

    exec(
        &engine,
        "CREATE TABLE returning_delete_event(e INTEGER PRIMARY KEY, x TEXT);
         CREATE TABLE returning_delete_target(i INTEGER PRIMARY KEY, y TEXT);
         INSERT INTO returning_delete_event VALUES (3, 'event-old');
         INSERT INTO returning_delete_target VALUES (3, 'action-old');
         CREATE RULE returning_delete_provider AS ON DELETE TO returning_delete_event DO INSTEAD
           DELETE FROM returning_delete_target AS trgt WHERE trgt.i = OLD.e
           RETURNING OLD.*",
    );
    let deleted = exec(
        &engine,
        "DELETE FROM returning_delete_event WHERE e = 3
         RETURNING e, x, old.x, new.x",
    );
    assert_eq!(deleted.value_at(0, 0), Some(&Value::Int(3)));
    assert_eq!(
        deleted.value_at(0, 1),
        Some(&Value::Str("event-old".into()))
    );
    assert_eq!(
        deleted.value_at(0, 2),
        Some(&Value::Str("event-old".into()))
    );
    assert_eq!(deleted.value_at(0, 3), Some(&Value::Null));
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM returning_delete_event").value_at(0, 0),
        Some(&Value::Int(1))
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM returning_delete_target").value_at(0, 0),
        Some(&Value::Int(0))
    );
}

#[test]
fn insert_action_old_star_takes_precedence_over_the_rule_event_row() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_insert_old_event(e INTEGER PRIMARY KEY, x TEXT);
         CREATE TABLE returning_insert_old_target(i INTEGER, y TEXT);
         INSERT INTO returning_insert_old_event VALUES (6, 'event-old');
         CREATE RULE returning_insert_old_provider AS ON UPDATE TO returning_insert_old_event DO INSTEAD
           INSERT INTO returning_insert_old_target VALUES (NEW.e + 10, NEW.x || '-action')
           RETURNING OLD.*",
    );
    let inserted_old = exec(
        &engine,
        "UPDATE returning_insert_old_event SET x = 'event-new'
         RETURNING e, x, old.x, new.x",
    );
    for position in 0..=2 {
        assert_eq!(inserted_old.value_at(0, position), Some(&Value::Null));
    }
    assert_eq!(
        inserted_old.value_at(0, 3),
        Some(&Value::Str("event-new-action".into()))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT i || ':' || y AS value FROM returning_insert_old_target"
        )
        .value_at(0, 0),
        Some(&Value::Str("16:event-new-action".into()))
    );
}

#[test]
fn explicit_action_returning_aliases_do_not_capture_rule_event_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_explicit_event(e INTEGER PRIMARY KEY, x TEXT);
         CREATE TABLE returning_explicit_target(i INTEGER PRIMARY KEY, y TEXT);
         INSERT INTO returning_explicit_event VALUES (4, 'event-old');
         INSERT INTO returning_explicit_target VALUES (4, 'action-old');
         CREATE RULE returning_explicit_provider AS ON UPDATE TO returning_explicit_event DO INSTEAD
           UPDATE returning_explicit_target AS trgt
           SET y = NEW.x || '-action' WHERE trgt.i = OLD.e
           RETURNING WITH (NEW AS action_new) action_new.*",
    );
    let updated = exec(
        &engine,
        "UPDATE returning_explicit_event SET x = 'event-new' WHERE e = 4
         RETURNING e, x, old.x, new.x",
    );
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(4)));
    assert_eq!(
        updated.value_at(0, 1),
        Some(&Value::Str("event-new-action".into()))
    );
    assert_eq!(
        updated.value_at(0, 2),
        Some(&Value::Str("action-old".into()))
    );
    assert_eq!(
        updated.value_at(0, 3),
        Some(&Value::Str("event-new-action".into()))
    );
}

#[test]
fn event_row_returning_stars_enforce_rule_event_sides_and_column_names() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_scope_event(e INTEGER, x TEXT);
         CREATE TABLE returning_scope_target(i INTEGER, y TEXT)",
    );
    let old = engine
        .sql(
            "CREATE RULE returning_invalid_old AS ON INSERT TO returning_scope_event DO INSTEAD
               UPDATE returning_scope_target AS trgt SET y = 'x' RETURNING OLD.*",
            &[],
        )
        .expect_err("an UPDATE action must expose the rule event namespace in RETURNING");
    assert_eq!(old.sqlstate(), Some("42P17"), "{old}");

    let new = engine
        .sql(
            "CREATE RULE returning_invalid_new AS ON DELETE TO returning_scope_event DO INSTEAD
               DELETE FROM returning_scope_target AS trgt RETURNING NEW.*",
            &[],
        )
        .expect_err("a DELETE action must expose the rule event namespace in RETURNING");
    assert_eq!(new.sqlstate(), Some("42P17"), "{new}");

    let missing_event = engine
        .sql(
            "CREATE RULE returning_missing_event AS ON UPDATE TO returning_scope_event DO INSTEAD
               UPDATE returning_scope_target AS trgt SET y = 'x' RETURNING NEW.missing, NEW.x",
            &[],
        )
        .expect_err("event-row fields must be checked against the event relation");
    assert_eq!(missing_event.sqlstate(), Some("42703"), "{missing_event}");

    let missing_action = engine
        .sql(
            "CREATE RULE returning_missing_action AS ON UPDATE TO returning_scope_event DO INSTEAD
               INSERT INTO returning_scope_target VALUES (NEW.e, NEW.x) RETURNING NEW.e, NEW.x",
            &[],
        )
        .expect_err("INSERT action images must be checked against the action target");
    assert_eq!(missing_action.sqlstate(), Some("42703"), "{missing_action}");
}

#[test]
fn action_returning_namespace_reports_postgresql_alias_conflicts() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_alias_event(e INTEGER, x TEXT);
         CREATE TABLE returning_alias_target(i INTEGER, y TEXT)",
    );

    exec(
        &engine,
        "CREATE RULE returning_unused_new AS ON UPDATE TO returning_alias_event DO ALSO
           UPDATE returning_alias_target AS new SET y = 'literal';
         CREATE RULE returning_unused_old AS ON DELETE TO returning_alias_event DO ALSO
           DELETE FROM returning_alias_target AS old",
    );

    let ambiguous_body = engine
        .sql(
            "CREATE RULE returning_ambiguous_body AS ON UPDATE TO returning_alias_event DO ALSO
               UPDATE returning_alias_target AS new SET y = new.y",
            &[],
        )
        .expect_err("an action target and rule event row cannot share a referenced qualifier");
    assert_eq!(ambiguous_body.sqlstate(), Some("42P09"), "{ambiguous_body}");

    exec(
        &engine,
        "CREATE RULE returning_nested_new AS ON UPDATE TO returning_alias_event DO ALSO
           UPDATE returning_alias_target AS new
           SET y = (SELECT new.x FROM (VALUES ('nested')) AS new(x))",
    );

    let ambiguous_new = engine
        .sql(
            "CREATE RULE returning_ambiguous_new AS ON UPDATE TO returning_alias_event DO INSTEAD
               UPDATE returning_alias_target AS new SET y = 'literal' RETURNING i",
            &[],
        )
        .expect_err("an UPDATE target cannot collide with the rule NEW relation");
    assert_eq!(ambiguous_new.sqlstate(), Some("42P09"), "{ambiguous_new}");
    assert!(ambiguous_new
        .to_string()
        .contains("table reference \"new\" is ambiguous"));

    let ambiguous_old = engine
        .sql(
            "CREATE RULE returning_ambiguous_old AS ON DELETE TO returning_alias_event DO INSTEAD
               DELETE FROM returning_alias_target AS old RETURNING i",
            &[],
        )
        .expect_err("a DELETE target cannot collide with the rule OLD relation");
    assert_eq!(ambiguous_old.sqlstate(), Some("42P09"), "{ambiguous_old}");
    assert!(ambiguous_old
        .to_string()
        .contains("table reference \"old\" is ambiguous"));

    let inaccessible = engine
        .sql(
            "CREATE RULE returning_inaccessible_event AS ON UPDATE TO returning_alias_event DO INSTEAD
               INSERT INTO returning_alias_target VALUES (NEW.e, NEW.x)
               RETURNING WITH (NEW AS action_new) NEW.*",
            &[],
        )
        .expect_err("an INSERT action must not expose the rule event row after renaming its image");
    assert_eq!(inaccessible.sqlstate(), Some("42P01"), "{inaccessible}");
    assert!(inaccessible
        .to_string()
        .contains("invalid reference to FROM-clause entry for table \"new\""));

    exec(
        &engine,
        "CREATE TABLE returning_insert_alias_event(e INTEGER, x TEXT);
         CREATE TABLE returning_insert_alias_target(i INTEGER, y TEXT);
         CREATE RULE returning_insert_alias_provider AS ON INSERT TO returning_insert_alias_event DO INSTEAD
           INSERT INTO returning_insert_alias_target AS new VALUES (NEW.e + 10, NEW.x || '-action')
           RETURNING new.*",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO returning_insert_alias_event VALUES (5, 'event') RETURNING e, x",
    );
    assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(15)));
    assert_eq!(
        inserted.value_at(0, 1),
        Some(&Value::Str("event-action".into()))
    );
}

#[test]
fn returning_only_event_references_keep_set_oriented_action_cardinality() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_cardinality_event(e INTEGER, x TEXT);
         CREATE TABLE returning_cardinality_target(i INTEGER, y TEXT);
         INSERT INTO returning_cardinality_event VALUES (1, 'one'), (2, 'two');
         INSERT INTO returning_cardinality_target VALUES (10, 'ten');
         CREATE RULE returning_cardinality_provider AS ON UPDATE TO returning_cardinality_event DO INSTEAD
           UPDATE returning_cardinality_target SET y = y RETURNING NEW.*",
    );
    let updated = exec(
        &engine,
        "UPDATE returning_cardinality_event SET x = x || '!' RETURNING e, x",
    );
    assert_eq!(updated.rows.len(), 1);
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(updated.value_at(0, 1), Some(&Value::Str("one!".into())));
    assert_eq!(
        strings(
            &engine,
            "SELECT e || ':' || x AS value FROM returning_cardinality_event ORDER BY e",
            "value",
        ),
        ["1:one", "2:two"]
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT i || ':' || y AS value FROM returning_cardinality_target",
            "value",
        ),
        ["10:ten"]
    );
}

#[test]
fn event_row_returning_columns_follow_rename_drop_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("rule-returning-event-row-lifecycle.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE returning_lifecycle_event(z INTEGER PRIMARY KEY, a TEXT);
             CREATE TABLE returning_lifecycle_target(i INTEGER PRIMARY KEY, y TEXT);
             INSERT INTO returning_lifecycle_event VALUES (1, 'event-old');
             INSERT INTO returning_lifecycle_target VALUES (1, 'action-old');
             CREATE RULE returning_lifecycle_provider AS ON UPDATE TO returning_lifecycle_event DO INSTEAD
               UPDATE returning_lifecycle_target AS trgt SET y = NEW.a WHERE trgt.i = OLD.z
               RETURNING NEW.*;
             ALTER TABLE returning_lifecycle_event RENAME COLUMN a TO renamed",
        );
        let updated = exec(
            &engine,
            "UPDATE returning_lifecycle_event SET renamed = 'event-new' RETURNING z, renamed",
        );
        assert_eq!(updated.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(
            updated.value_at(0, 1),
            Some(&Value::Str("event-new".into()))
        );
    }

    let engine = Engine::open(&database).expect("a bound event-row RETURNING list must restore");
    let updated = exec(
        &engine,
        "UPDATE returning_lifecycle_event SET renamed = 'after-reopen' RETURNING z, renamed",
    );
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(
        updated.value_at(0, 1),
        Some(&Value::Str("after-reopen".into()))
    );
    assert_eq!(
        exec(&engine, "SELECT y FROM returning_lifecycle_target").value_at(0, 0),
        Some(&Value::Str("after-reopen".into()))
    );
    let dependent = engine
        .sql(
            "ALTER TABLE returning_lifecycle_event DROP COLUMN renamed",
            &[],
        )
        .expect_err("the expanded RETURNING list must retain its event-column dependency");
    assert_eq!(dependent.sqlstate(), Some("2BP01"), "{dependent}");
    exec(
        &engine,
        "ALTER TABLE returning_lifecycle_event DROP COLUMN renamed CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE rulename = 'returning_lifecycle_provider'",
    )
    .rows
    .is_empty());
}

#[test]
fn event_row_returning_star_keeps_creation_width_after_add_column() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_add_event(z INTEGER PRIMARY KEY, a TEXT);
         CREATE TABLE returning_add_target(i INTEGER PRIMARY KEY, y TEXT);
         INSERT INTO returning_add_event VALUES (1, 'event-old');
         INSERT INTO returning_add_target VALUES (1, 'action-old');
         CREATE RULE returning_add_provider AS ON UPDATE TO returning_add_event DO INSTEAD
           UPDATE returning_add_target AS trgt SET y = NEW.a WHERE trgt.i = OLD.z
           RETURNING NEW.*;
         ALTER TABLE returning_add_event ADD COLUMN later INTEGER DEFAULT 7",
    );
    let mismatch = engine
        .sql(
            "UPDATE returning_add_event SET a = 'event-new' RETURNING *",
            &[],
        )
        .expect_err("a creation-time RETURNING star cannot synthesize a later event column");
    assert_eq!(mismatch.sqlstate(), Some("XX000"), "{mismatch}");
    assert!(
        mismatch
            .to_string()
            .contains("could not find replacement targetlist entry for attno 3"),
        "{mismatch}"
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT z || ':' || a || ':' || later AS value FROM returning_add_event",
        )
        .value_at(0, 0),
        Some(&Value::Str("1:event-old:7".into()))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT i || ':' || y AS value FROM returning_add_target",
        )
        .value_at(0, 0),
        Some(&Value::Str("1:action-old".into()))
    );
}
