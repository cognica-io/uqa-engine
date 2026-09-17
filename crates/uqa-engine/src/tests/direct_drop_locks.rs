//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct relation deletion retains SQL definition locks and dependency checks.

use super::relation_lock_support::{after_operation_wait, sessions, sql};
use crate::Engine;
use uqa_core::Value;
use uqa_sql::{catalog::errors::storage_error, SQLError};

#[derive(Clone, Copy)]
enum Kind {
    Table,
    Foreign,
}
impl Kind {
    fn keyword(self) -> &'static str {
        match self {
            Self::Table => "TABLE",
            Self::Foreign => "FOREIGN TABLE",
        }
    }
    fn create(self, engine: &Engine) {
        sql(
            engine,
            match self {
                Self::Table => "CREATE TABLE v(value integer)",
                Self::Foreign => "CREATE FOREIGN TABLE v(value integer) SERVER source",
            },
        );
    }
    fn drop(self, engine: &Engine) -> Result<bool, SQLError> {
        match self {
            Self::Table => engine
                .drop_table("v")
                .map_err(|error| storage_error("direct DROP TABLE", &error)),
            Self::Foreign => engine.drop_foreign_table("v").map_err(SQLError::Internal),
        }
    }
}

#[test]
fn direct_table_drops_wait_and_recheck_owner_name_and_replacement() {
    for provider in 0..3 {
        for kind in [Kind::Table, Kind::Foreign] {
            for scenario in 0..4 {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
                );
                kind.create(&first);
                let keyword = kind.keyword();
                match scenario {
                    0 => {
                        sql(
                            &first,
                            &format!("BEGIN; ALTER {keyword} v OWNER TO CURRENT_USER"),
                        );
                    }
                    1 | 3 => {
                        sql(&first, &format!("BEGIN; ALTER {keyword} v RENAME TO moved"));
                        if scenario == 3 {
                            kind.create(&first);
                        }
                    }
                    _ => {
                        sql(&first, &format!("CREATE ROLE before_owner; CREATE ROLE after_owner; GRANT CREATE ON SCHEMA public TO before_owner, after_owner; ALTER {keyword} v OWNER TO before_owner"));
                        sql(&second, "SET ROLE before_owner");
                        sql(
                            &first,
                            &format!("BEGIN; ALTER {keyword} v OWNER TO after_owner"),
                        );
                    }
                }
                let (second, result) =
                    after_operation_wait(&first, second, "public.v", "COMMIT", move |engine| {
                        kind.drop(engine)
                    });
                if scenario == 2 {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains("must be owner"), "{error}");
                    if matches!(kind, Kind::Table) {
                        assert_eq!(error.sqlstate(), Some("42501"));
                    }
                    assert_ne!(
                        sql(&first, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                        Value::Null
                    );
                } else {
                    assert_eq!(result.unwrap(), scenario != 1);
                    assert_eq!(
                        sql(&second, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                        Value::Null
                    );
                    if scenario == 1 || scenario == 3 {
                        assert_ne!(
                            sql(&second, "SELECT to_regclass('moved') AS relation").rows[0]
                                ["relation"],
                            Value::Null
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn direct_table_drops_recheck_waited_view_edges_and_reject_retained_dependencies() {
    for provider in 0..3 {
        for kind in [Kind::Table, Kind::Foreign] {
            for removed_edge in [false, true] {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
                );
                kind.create(&first);
                sql(&first, "CREATE VIEW d AS SELECT value FROM v; CREATE VIEW outer_view AS SELECT * FROM d");
                sql(
                    &first,
                    if removed_edge {
                        "BEGIN; CREATE OR REPLACE VIEW d AS SELECT 2 AS value"
                    } else {
                        "BEGIN; ALTER VIEW d SET (security_barrier=true)"
                    },
                );
                let (second, result) =
                    after_operation_wait(&first, second, "public.d", "COMMIT", move |engine| {
                        kind.drop(engine)
                    });
                if removed_edge {
                    assert!(result.unwrap());
                    assert_eq!(
                        sql(&second, "SELECT * FROM outer_view").rows[0]["value"],
                        Value::Int(2)
                    );
                } else {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains("view public.d"), "{error}");
                    if matches!(kind, Kind::Table) {
                        assert_eq!(error.sqlstate(), Some("2BP01"));
                    }
                    assert_ne!(
                        sql(&second, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                        Value::Null
                    );
                }
            }
        }
    }
}

#[test]
fn direct_table_drops_share_routine_preflight_read_only_rules_and_missing_results() {
    fn verify(engine: &Engine) {
        sql(
            engine,
            "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
        );
        for kind in [Kind::Table, Kind::Foreign] {
            kind.create(engine);
            sql(engine, "CREATE FUNCTION dependent() RETURNS bigint LANGUAGE SQL RETURN (SELECT count(*) FROM v)");
            let error = kind.drop(engine).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("because other objects depend on it"),
                "{error}"
            );
            if matches!(kind, Kind::Table) {
                assert_eq!(error.sqlstate(), Some("2BP01"));
            }
            assert_ne!(
                sql(engine, "SELECT to_regclass('v') AS relation").rows[0]["relation"],
                Value::Null
            );
            sql(engine, "DROP FUNCTION dependent()");
            sql(engine, "BEGIN READ ONLY");
            let error = kind.drop(engine).unwrap_err();
            assert!(error.to_string().contains("read-only"), "{error}");
            if matches!(kind, Kind::Table) {
                assert_eq!(error.sqlstate(), Some("25006"));
            }
            sql(engine, "ROLLBACK");
            assert!(kind.drop(engine).unwrap());
            engine.query_runtime_view().notices.lock().clear();
            assert!(!kind.drop(engine).unwrap());
            assert!(engine.query_runtime_view().notices.lock().is_empty());
            sql(engine, "CREATE VIEW v AS SELECT 1 AS value");
            let error = kind.drop(engine).unwrap_err();
            assert!(error.to_string().contains("not a"), "{error}");
            sql(engine, "DROP VIEW v");
        }
    }
    verify(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        verify(&engine);
    }
}
