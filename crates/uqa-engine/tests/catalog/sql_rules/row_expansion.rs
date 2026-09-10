//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rule_row_stars_expand_values_and_select_in_event_column_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE star_update_source(z INTEGER, a INTEGER DEFAULT 0);
         CREATE TABLE star_update_log(seq BIGSERIAL PRIMARY KEY, z INTEGER, a INTEGER, tag TEXT);
         INSERT INTO star_update_source VALUES (1, 2), (11, 12);
         CREATE RULE star_update_rule AS ON UPDATE TO star_update_source DO ALSO
           INSERT INTO star_update_log(z, a, tag)
           VALUES (OLD.*, 'old'), (NEW.*, 'new')",
    );
    exec(&engine, "UPDATE star_update_source SET a = a + 1");
    assert_eq!(
        strings(
            &engine,
            "SELECT z || ':' || a || ':' || tag AS value FROM star_update_log ORDER BY seq",
            "value",
        ),
        ["1:2:old", "1:3:new", "11:12:old", "11:13:new"]
    );

    exec(
        &engine,
        "CREATE TABLE star_insert_source(z INTEGER, a INTEGER DEFAULT 0);
         CREATE TABLE star_insert_log(seq BIGSERIAL PRIMARY KEY, z INTEGER, a INTEGER, tag TEXT);
         CREATE RULE star_insert_rule AS ON INSERT TO star_insert_source DO ALSO
           INSERT INTO star_insert_log(z, a, tag)
           VALUES (NULL, NULL, 'constant'), (NEW.*, 'new')",
    );
    exec(
        &engine,
        "INSERT INTO star_insert_source VALUES (22, 23), (33, DEFAULT)",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT coalesce(z::text, '-') || ':' || coalesce(a::text, '-') || ':' || tag AS value FROM star_insert_log ORDER BY seq",
            "value",
        ),
        ["-:-:constant", "22:23:new", "-:-:constant", "33:0:new"]
    );

    exec(
        &engine,
        "CREATE TABLE star_redirect_source(z INTEGER, a INTEGER DEFAULT 0);
         CREATE TABLE star_redirect_target(z INTEGER, a INTEGER);
         CREATE RULE star_redirect_rule AS ON INSERT TO star_redirect_source DO INSTEAD
           INSERT INTO star_redirect_target SELECT NEW.*",
    );
    exec(
        &engine,
        "INSERT INTO star_redirect_source VALUES (41, 42), (51, DEFAULT)",
    );
    assert!(exec(&engine, "SELECT * FROM star_redirect_source")
        .rows
        .is_empty());
    assert_eq!(
        strings(
            &engine,
            "SELECT z || ':' || a AS value FROM star_redirect_target ORDER BY z",
            "value",
        ),
        ["41:42", "51:0"]
    );

    exec(
        &engine,
        "CREATE TABLE star_row_source(z INTEGER, a TEXT);
         CREATE TABLE star_row_log(seq BIGSERIAL PRIMARY KEY, matched BOOLEAN);
         CREATE RULE star_row_rule AS ON INSERT TO star_row_source DO ALSO
           INSERT INTO star_row_log(matched) VALUES (ROW(NEW.*) = ROW(5, 'five'));
         INSERT INTO star_row_source VALUES (5, 'five'), (6, 'other')",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT matched::text AS value FROM star_row_log ORDER BY seq",
            "value",
        ),
        ["true", "false"]
    );
}

#[test]
fn rule_row_star_expansion_is_creation_time_stable_across_lifecycle_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rule-row-star-lifecycle.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE star_lifecycle_source(z INTEGER, a TEXT);
             CREATE TABLE star_lifecycle_target(z INTEGER, a TEXT);
             CREATE RULE star_lifecycle_rule AS ON INSERT TO star_lifecycle_source DO ALSO
               INSERT INTO star_lifecycle_target VALUES (NEW.*);
             ALTER TABLE star_lifecycle_source ADD COLUMN later INTEGER DEFAULT 7;
             INSERT INTO star_lifecycle_source VALUES (1, 'one', 9)",
        );
        assert_eq!(
            strings(
                &engine,
                "SELECT z || ':' || a AS value FROM star_lifecycle_target",
                "value",
            ),
            ["1:one"]
        );
        let dependent = engine
            .sql("ALTER TABLE star_lifecycle_source DROP COLUMN a", &[])
            .expect_err("the expanded star must retain every creation-time column dependency");
        assert_eq!(dependent.sqlstate(), Some("2BP01"), "{dependent}");
        exec(
            &engine,
            "ALTER TABLE star_lifecycle_source RENAME COLUMN a TO renamed;
             INSERT INTO star_lifecycle_source VALUES (2, 'two', 10)",
        );
    }

    let engine = Engine::open(&database).unwrap();
    exec(
        &engine,
        "INSERT INTO star_lifecycle_source VALUES (3, 'three', 11)",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT z || ':' || a AS value FROM star_lifecycle_target ORDER BY z",
            "value",
        ),
        ["1:one", "2:two", "3:three"]
    );
    exec(
        &engine,
        "ALTER TABLE star_lifecycle_source DROP COLUMN renamed CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE rulename = 'star_lifecycle_rule'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_row_stars_enforce_event_sides_without_capturing_nested_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE star_scope_source(z INTEGER, a TEXT);
         CREATE TABLE star_scope_target(z INTEGER, a TEXT)",
    );
    let old = engine
        .sql(
            "CREATE RULE star_invalid_old AS ON INSERT TO star_scope_source DO ALSO
               INSERT INTO star_scope_target VALUES (OLD.*)",
            &[],
        )
        .expect_err("an INSERT rule cannot expand OLD");
    assert_eq!(old.sqlstate(), Some("42P17"), "{old}");
    assert!(old.to_string().contains("ON INSERT rule cannot use OLD"));

    let new = engine
        .sql(
            "CREATE RULE star_invalid_new AS ON DELETE TO star_scope_source DO ALSO
               INSERT INTO star_scope_target VALUES (NEW.*)",
            &[],
        )
        .expect_err("a DELETE rule cannot expand NEW");
    assert_eq!(new.sqlstate(), Some("42P17"), "{new}");
    assert!(new.to_string().contains("ON DELETE rule cannot use NEW"));

    exec(
        &engine,
        "CREATE RULE star_nested_alias AS ON INSERT TO star_scope_source DO ALSO
           INSERT INTO star_scope_target
           SELECT nested.* FROM (
             SELECT old.* FROM (VALUES (9, 'local')) AS old(z, a)
           ) AS nested;
         INSERT INTO star_scope_source VALUES (1, 'event')",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT z || ':' || a AS value FROM star_scope_target",
            "value",
        ),
        ["9:local"]
    );
}

#[test]
fn expanded_rule_row_stars_obey_cte_and_set_operation_scope_restrictions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE star_restricted_source(id INTEGER, note TEXT);
         CREATE TABLE star_restricted_target(id INTEGER, note TEXT)",
    );
    let cte = engine
        .sql(
            "CREATE RULE star_cte_rule AS ON INSERT TO star_restricted_source DO ALSO
               WITH item AS (SELECT NEW.*)
               INSERT INTO star_restricted_target SELECT * FROM item",
            &[],
        )
        .expect_err("a rule CTE cannot capture the event row through a star");
    assert_eq!(cte.sqlstate(), Some("0A000"), "{cte}");

    let set_operation = engine
        .sql(
            "CREATE RULE star_set_rule AS ON INSERT TO star_restricted_source DO ALSO
               INSERT INTO star_restricted_target
               SELECT NEW.* UNION ALL SELECT 99, 'constant'",
            &[],
        )
        .expect_err("a set-operation member cannot capture the event row through a star");
    assert_eq!(set_operation.sqlstate(), Some("42P10"), "{set_operation}");
}
