//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation ALTER authority is checked before kind and again after definition waits.

use crate::tests::relation_lock_support::{after_wait, sessions, sql};
use crate::Engine;

fn rejection(engine: &Engine, statement: &str, state: &str, message: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{statement}: {error}");
    assert!(error.to_string().contains(message), "{statement}: {error}");
}

#[test]
fn relation_alteration_checks_actual_ownership_before_requested_kind() {
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE TABLE s.wrong(id integer); SET ROLE reader");
        for (kind, option) in [
            ("VIEW", "SET (security_barrier=true)"),
            ("MATERIALIZED VIEW", "SET (fillfactor=80)"),
            ("FOREIGN TABLE", "OWNER TO reader"),
        ] {
            for action in ["OWNER TO reader", "RENAME TO renamed", option] {
                rejection(
                    &engine,
                    &format!("ALTER {kind} s.wrong {action}"),
                    "42501",
                    "must be owner of table wrong",
                );
            }
        }
        sql(&engine, "RESET ROLE; ALTER TABLE s.wrong OWNER TO reader; REVOKE CREATE ON SCHEMA s FROM reader; SET ROLE reader");
        for kind in ["VIEW", "MATERIALIZED VIEW", "FOREIGN TABLE"] {
            rejection(
                &engine,
                &format!("ALTER {kind} s.wrong OWNER TO reader"),
                "42809",
                &format!("\"wrong\" is not a {}", kind.to_lowercase()),
            );
            rejection(
                &engine,
                &format!("ALTER {kind} s.wrong RENAME TO renamed"),
                "42501",
                "permission denied for schema s",
            );
        }
        sql(
            &engine,
            "RESET ROLE; GRANT CREATE ON SCHEMA s TO reader; SET ROLE reader",
        );
        for kind in ["VIEW", "MATERIALIZED VIEW", "FOREIGN TABLE"] {
            rejection(
                &engine,
                &format!("ALTER {kind} s.wrong RENAME TO renamed"),
                "42809",
                &format!("\"wrong\" is not a {}", kind.to_lowercase()),
            );
        }
    }
}

#[test]
fn view_and_foreign_rename_require_source_create_without_restricting_options() {
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader; CREATE VIEW s.v AS SELECT 1 AS id; CREATE MATERIALIZED VIEW s.m AS SELECT 1 AS id; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE s.f(id integer) SERVER remote; ALTER VIEW s.v OWNER TO reader; ALTER MATERIALIZED VIEW s.m OWNER TO reader; ALTER FOREIGN TABLE s.f OWNER TO reader; REVOKE CREATE ON SCHEMA s FROM reader; SET ROLE reader");
        for (kind, name) in [
            ("VIEW", "v"),
            ("MATERIALIZED VIEW", "m"),
            ("FOREIGN TABLE", "f"),
        ] {
            rejection(
                &engine,
                &format!("ALTER {kind} s.{name} RENAME TO renamed"),
                "42501",
                "permission denied for schema s",
            );
            sql(&engine, &format!("ALTER {kind} s.{name} OWNER TO reader"));
        }
        sql(&engine, "ALTER VIEW s.v SET (security_barrier=true); ALTER MATERIALIZED VIEW s.m SET (fillfactor=80)");
    }
}

#[test]
fn relation_rename_rechecks_source_create_after_definition_waits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for (kind, create, hold) in [
                ("VIEW", "CREATE VIEW s.v AS SELECT 1 AS id", "ALTER VIEW s.v SET (security_barrier=true)"),
                ("MATERIALIZED VIEW", "CREATE MATERIALIZED VIEW s.v AS SELECT 1 AS id", "ALTER MATERIALIZED VIEW s.v SET (fillfactor=80)"),
                ("FOREIGN TABLE", "CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE s.v(id integer) SERVER remote", "ALTER FOREIGN TABLE s.v OWNER TO reader"),
            ] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader");
                sql(&first, create);
                sql(&first, &format!("ALTER {kind} s.v OWNER TO reader"));
                sql(&second, &format!("SET ROLE reader; BEGIN ISOLATION LEVEL {isolation}; SELECT 1"));
                sql(&first, &format!("BEGIN; {hold}; REVOKE CREATE ON SCHEMA s FROM reader"));
                let (second, result) = after_wait(&first, second, &format!("ALTER {kind} s.v RENAME TO renamed"), "s.v", "COMMIT");
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("42501"), "{provider}/{isolation}/{kind}: {error}");
                assert!(error.to_string().contains("permission denied for schema s"));
                sql(&second, "ROLLBACK");
            }
        }
    }
}

#[test]
fn temporary_view_rename_requires_current_database_temp_authority() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; REVOKE TEMP ON DATABASE uqa FROM PUBLIC; GRANT TEMP ON DATABASE uqa TO reader; SET ROLE reader; CREATE TEMP VIEW v AS SELECT 1 AS id; RESET ROLE; REVOKE TEMP ON DATABASE uqa FROM reader; SET ROLE reader");
    rejection(
        &engine,
        "ALTER VIEW pg_temp.v RENAME TO renamed",
        "42501",
        "permission denied for schema pg_temp_",
    );
    sql(&engine, "ALTER VIEW pg_temp.v SET (security_barrier=true)");
}

#[test]
fn relation_alteration_rechecks_actual_owner_and_kind_after_replacement() {
    for provider in 0..3 {
        for (kind, create) in [
            ("VIEW", "CREATE VIEW s.v AS SELECT 1 AS id"),
            ("MATERIALIZED VIEW", "CREATE MATERIALIZED VIEW s.v AS SELECT 1 AS id"),
            ("FOREIGN TABLE", "CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE s.v(id integer) SERVER remote"),
        ] {
            for owned in [false, true] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; GRANT USAGE, CREATE ON SCHEMA s TO reader");
                sql(&first, create);
                sql(&first, &format!("ALTER {kind} s.v OWNER TO reader"));
                sql(&second, "SET ROLE reader");
                sql(&first, &format!("BEGIN; DROP {kind} s.v; CREATE TABLE s.v(id integer); INSERT INTO s.v VALUES (2)"));
                if owned {
                    sql(&first, "ALTER TABLE s.v OWNER TO reader");
                }
                let (_, result) = after_wait(&first, second, &format!("ALTER {kind} s.v RENAME TO renamed"), "s.v", "COMMIT");
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some(if owned { "42809" } else { "42501" }), "{provider}/{kind}/{owned}: {error}");
                let message = if owned {
                    format!("\"v\" is not a {}", kind.to_lowercase())
                } else {
                    "must be owner of table v".into()
                };
                assert!(error.to_string().contains(&message), "{error}");
                assert_eq!(sql(&first, "SELECT id FROM s.v").rows[0]["id"], uqa_core::Value::Int(2));
            }
        }
    }
}

#[test]
fn relation_alteration_checks_catalog_ownership_before_system_protection_and_kind() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; SET ROLE reader");
    for kind in ["VIEW", "MATERIALIZED VIEW", "FOREIGN TABLE"] {
        rejection(
            &engine,
            &format!("ALTER {kind} pg_catalog.pg_class RENAME TO renamed"),
            "42501",
            "must be owner of table pg_class",
        );
    }
    sql(&engine, "RESET ROLE");
    for kind in ["VIEW", "MATERIALIZED VIEW", "FOREIGN TABLE"] {
        rejection(
            &engine,
            &format!("ALTER {kind} pg_catalog.pg_class RENAME TO renamed"),
            "42501",
            "is a system catalog",
        );
    }
}
