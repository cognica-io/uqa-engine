//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted system ACLs share query, lock, inquiry and transaction visibility.

use super::{
    relation_lock_support::{after_tuple_wait, after_wait, error, sessions, sql},
    *,
};

fn boolean(engine: &Engine, expression: &str, expected: bool) {
    assert_eq!(
        sql(engine, &format!("SELECT {expression} AS result")).rows[0]["result"],
        Value::Bool(expected),
        "{expression}"
    );
}
fn maintain(engine: &Engine, role: &str, table: &str, expected: bool) {
    boolean(
        engine,
        &format!("has_table_privilege('{role}','{table}','MAINTAIN')"),
        expected,
    );
}
fn reopen(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[test]
fn system_catalog_security_inquiries_match_postgresql_masks_and_attribute_numbers() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; GRANT UPDATE ON pg_class TO reader WITH GRANT OPTION; GRANT UPDATE(relname) ON pg_class TO reader");
    for (expression, expected) in [
        ("has_table_privilege('reader','pg_class','UPDATE')", false),
        (
            "has_table_privilege('reader','pg_class','UPDATE WITH GRANT OPTION')",
            true,
        ),
        (
            "has_column_privilege('reader','pg_class','relname','UPDATE')",
            true,
        ),
        (
            "has_column_privilege('reader','pg_class','relnamespace','UPDATE')",
            false,
        ),
        (
            "has_column_privilege('reader',1259,2::smallint,'UPDATE')",
            true,
        ),
        (
            "has_column_privilege('reader',1259,-1::smallint,'UPDATE')",
            false,
        ),
        (
            "has_column_privilege('reader',1259,0::smallint,'SELECT') IS NULL",
            true,
        ),
        ("has_table_privilege('reader',1260,'SELECT')", false),
        (
            "has_column_privilege('reader',1260,1::smallint,'SELECT')",
            false,
        ),
    ] {
        boolean(&engine, expression, expected);
    }
    sql(&engine, "SET ROLE reader; BEGIN");
    error(&engine, "LOCK pg_class IN ROW SHARE MODE", "42501");
    sql(&engine, "ROLLBACK; RESET ROLE; GRANT MAINTAIN ON pg_class TO reader; SET ROLE reader; BEGIN; LOCK pg_class IN ACCESS EXCLUSIVE MODE; ROLLBACK");
}

#[test]
fn system_catalog_security_controls_queries_prepared_plans_and_view_subjects() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE VIEW catalog_names AS SELECT relname FROM pg_class; GRANT SELECT ON catalog_names TO reader; REVOKE SELECT ON pg_class FROM PUBLIC; GRANT SELECT(relname) ON pg_class TO reader; SET ROLE reader");
    sql(&engine, "SELECT relname FROM pg_class; SELECT relname FROM catalog_names; PREPARE names AS SELECT relname FROM pg_class; EXECUTE names");
    error(&engine, "SELECT relnamespace FROM pg_class", "42501");
    error(&engine, "SELECT * FROM pg_class LIMIT 0", "42501");
    sql(
        &engine,
        "RESET ROLE; REVOKE SELECT(relname) ON pg_class FROM reader; SET ROLE reader",
    );
    error(&engine, "EXECUTE names", "42501");
    sql(&engine, "SELECT relname FROM catalog_names");
    sql(&engine, "BEGIN");
    error(&engine, "LOCK pg_class IN ACCESS SHARE MODE", "42501");
    sql(
        &engine,
        "ROLLBACK; RESET ROLE; GRANT SELECT ON pg_class TO PUBLIC; SET ROLE reader; EXECUTE names",
    );
}

#[test]
fn system_catalog_security_persists_commits_and_undo_across_all_providers() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader");
        sql(&first, "BEGIN; GRANT MAINTAIN ON pg_class TO reader; SAVEPOINT acl; GRANT MAINTAIN ON pg_type TO reader");
        maintain(&first, "reader", "pg_class", true);
        maintain(&second, "reader", "pg_class", false);
        sql(&first, "ROLLBACK TO acl");
        maintain(&first, "reader", "pg_type", false);
        sql(&first, "COMMIT");
        maintain(&second, "reader", "pg_class", true);
        sql(
            &first,
            "BEGIN; REVOKE MAINTAIN ON pg_class FROM reader; ROLLBACK",
        );
        maintain(&first, "reader", "pg_class", true);
        sql(
            &first,
            "GRANT SELECT(relname) ON pg_class TO reader; REVOKE SELECT ON pg_class FROM PUBLIC",
        );
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        maintain(&reopened, "reader", "pg_class", true);
        maintain(&reopened, "reader", "pg_type", false);
        boolean(
            &reopened,
            "has_column_privilege('reader','pg_class','relname','SELECT')",
            true,
        );
        boolean(
            &reopened,
            "has_table_privilege('reader','pg_class','SELECT')",
            false,
        );
    }
}

#[test]
fn system_catalog_security_fixed_transactions_refresh_external_acls_and_retain_private_ones() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader");
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&second, "GRANT MAINTAIN ON pg_class TO reader");
            maintain(&first, "reader", "pg_class", true);
            sql(&second, "REVOKE MAINTAIN ON pg_class FROM reader");
            maintain(&first, "reader", "pg_class", false);
            sql(&first, "GRANT MAINTAIN ON pg_type TO reader; SAVEPOINT private_acl; GRANT MAINTAIN ON pg_proc TO reader");
            sql(&second, "GRANT MAINTAIN ON pg_class TO reader");
            maintain(&first, "reader", "pg_type", true);
            maintain(&first, "reader", "pg_class", true);
            sql(&first, "ROLLBACK TO private_acl");
            maintain(&first, "reader", "pg_proc", false);
            maintain(&first, "reader", "pg_type", true);
            sql(&first, "COMMIT");
            maintain(&second, "reader", "pg_type", true);
        }
    }
}

#[test]
fn system_catalog_security_independent_relations_and_columns_stage_and_commit_together() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader");
        sql(&first, "BEGIN; GRANT MAINTAIN ON pg_class TO reader");
        sql(
            &second,
            "BEGIN; GRANT MAINTAIN ON pg_type TO reader; COMMIT",
        );
        sql(&first, "COMMIT");
        maintain(&first, "reader", "pg_type", true);
        maintain(&second, "reader", "pg_class", true);
        sql(&first, "BEGIN; GRANT UPDATE(relname) ON pg_class TO reader");
        sql(
            &second,
            "BEGIN; GRANT UPDATE(relnamespace) ON pg_class TO reader; COMMIT",
        );
        sql(&first, "COMMIT");
        for column in ["relname", "relnamespace"] {
            boolean(
                &first,
                &format!("has_column_privilege('reader','pg_class','{column}','UPDATE')"),
                true,
            );
        }
    }
}

#[test]
fn system_catalog_security_conflicting_commits_error_and_aborts_release_tuple_waiters() {
    for provider in 0..3 {
        for release in ["COMMIT", "ROLLBACK", "ROLLBACK TO acl; COMMIT"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other");
            sql(
                &first,
                "BEGIN; SAVEPOINT acl; GRANT MAINTAIN ON pg_proc TO reader",
            );
            let (_, result) = after_tuple_wait(
                &first,
                second,
                "GRANT TRIGGER ON pg_proc TO other",
                "pg_catalog.pg_class",
                1255,
                release,
            );
            if release == "COMMIT" {
                assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
            } else {
                result.unwrap();
            }
        }
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; GRANT MAINTAIN ON pg_proc TO reader",
        );
        sql(
            &first,
            "BEGIN; REVOKE MAINTAIN ON pg_proc FROM reader; GRANT MAINTAIN ON pg_proc TO reader",
        );
        let (_, result) = after_tuple_wait(
            &first,
            second,
            "GRANT TRIGGER ON pg_proc TO reader",
            "pg_catalog.pg_class",
            1255,
            "COMMIT",
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
    }
}

#[test]
fn system_catalog_security_grants_wait_before_writer_admission_and_recheck_privileges() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; GRANT MAINTAIN ON pg_class TO reader",
        );
        sql(&first, "BEGIN; LOCK pg_class IN ACCESS EXCLUSIVE MODE; REVOKE MAINTAIN ON pg_class FROM reader");
        sql(&second, "SET ROLE reader; BEGIN");
        let (second, result) = after_wait(
            &first,
            second,
            "LOCK pg_class IN SHARE MODE",
            "pg_catalog.pg_class",
            "COMMIT",
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
        sql(&second, "ROLLBACK; RESET ROLE");
        sql(&first, "BEGIN; LOCK pg_attribute IN ACCESS EXCLUSIVE MODE");
        let (_, result) = after_wait(
            &first,
            second,
            "GRANT MAINTAIN ON pg_type TO reader",
            "pg_catalog.pg_attribute",
            "COMMIT",
        );
        result.unwrap();
    }
}

#[test]
fn system_catalog_security_role_dependencies_and_failed_mixed_grants_are_atomic() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE ROLE other; CREATE TABLE ordinary(v integer); GRANT SELECT(rolname) ON pg_authid TO reader WITH GRANT OPTION; SET ROLE reader; GRANT SELECT(rolname) ON pg_authid TO other; RESET ROLE");
    error(&engine, "DROP ROLE reader", "2BP01");
    error(
        &engine,
        "REVOKE SELECT(rolname) ON pg_authid FROM reader RESTRICT",
        "2BP01",
    );
    boolean(
        &engine,
        "has_column_privilege('other','pg_authid','rolname','SELECT')",
        true,
    );
    sql(
        &engine,
        "REVOKE SELECT(rolname) ON pg_authid FROM reader CASCADE; DROP ROLE reader",
    );
    boolean(
        &engine,
        "has_column_privilege('other','pg_authid','rolname','SELECT')",
        false,
    );
    error(
        &engine,
        "GRANT SELECT(v) ON ordinary, pg_class TO other",
        "42703",
    );
    boolean(
        &engine,
        "has_column_privilege('other','ordinary','v','SELECT')",
        false,
    );
    error(&engine, "GRANT MAINTAIN ON pg_class TO absent", "42704");
    sql(
        &engine,
        "GRANT SELECT ON ALL TABLES IN SCHEMA information_schema TO other",
    );
    boolean(
        &engine,
        "has_table_privilege('other','information_schema.enabled_roles','SELECT')",
        true,
    );
}

#[test]
fn system_catalog_security_column_tuple_waits_match_commit_and_savepoint_undo() {
    for provider in 0..3 {
        for release in ["COMMIT", "ROLLBACK", "ROLLBACK TO acl; COMMIT"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other");
            sql(
                &first,
                "BEGIN; SAVEPOINT acl; GRANT UPDATE(relname) ON pg_class TO reader",
            );
            let (_, result) = after_tuple_wait(
                &first,
                second,
                "GRANT UPDATE(relname) ON pg_class TO other",
                "pg_catalog.pg_attribute",
                (1259 << 16) | 2,
                release,
            );
            if release == "COMMIT" {
                assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn system_catalog_security_queries_recheck_acl_after_relation_waits() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader");
        sql(&second, "SET ROLE reader");
        sql(
            &first,
            "BEGIN; LOCK pg_class IN ACCESS EXCLUSIVE MODE; REVOKE SELECT ON pg_class FROM PUBLIC",
        );
        let (_, result) = after_wait(
            &first,
            second,
            "SELECT relname FROM pg_class",
            "pg_catalog.pg_class",
            "COMMIT",
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
    }
}

#[test]
fn system_catalog_security_fixed_snapshots_merge_private_and_external_attribute_grants() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader");
        sql(
            &first,
            "BEGIN ISOLATION LEVEL REPEATABLE READ; GRANT UPDATE(relname) ON pg_class TO reader",
        );
        sql(&second, "GRANT UPDATE(relnamespace) ON pg_class TO reader");
        for column in ["relname", "relnamespace"] {
            boolean(
                &first,
                &format!("has_column_privilege('reader','pg_class','{column}','UPDATE')"),
                true,
            );
        }
        sql(
            &second,
            "REVOKE UPDATE(relnamespace) ON pg_class FROM reader",
        );
        boolean(
            &first,
            "has_column_privilege('reader','pg_class','relnamespace','UPDATE')",
            false,
        );
        boolean(
            &first,
            "has_column_privilege('reader','pg_class','relname','UPDATE')",
            true,
        );
        sql(&first, "COMMIT");
        boolean(
            &second,
            "has_column_privilege('reader','pg_class','relname','UPDATE')",
            true,
        );
    }
}
