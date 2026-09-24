//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! System catalog roots and view sources share transaction-owned locks across providers.

use super::{
    relation_lock_support::{after_wait, error, sessions, sql},
    *,
};
use uqa_execution::row_locks::RelationLockMode;
use uqa_sql::catalog::VirtualRelation;

// Independent PostgreSQL 18.4 LOCK reference sets, excluding catalog index scan locks.
const CATALOGS: &[(&str, &str)] = &[
    ("pg_catalog.pg_tablespace", "pg_catalog.pg_tablespace"),
    ("pg_catalog.pg_type", "pg_catalog.pg_type"),
    ("pg_catalog.pg_attribute", "pg_catalog.pg_attribute"),
    ("pg_catalog.pg_proc", "pg_catalog.pg_proc"),
    ("pg_catalog.pg_class", "pg_catalog.pg_class"),
    ("pg_catalog.pg_authid", "pg_catalog.pg_authid"),
    ("pg_catalog.pg_auth_members", "pg_catalog.pg_auth_members"),
    ("pg_catalog.pg_database", "pg_catalog.pg_database"),
    ("pg_catalog.pg_sequence", "pg_catalog.pg_sequence"),
    ("pg_catalog.pg_attrdef", "pg_catalog.pg_attrdef"),
    ("pg_catalog.pg_constraint", "pg_catalog.pg_constraint"),
    ("pg_catalog.pg_depend", "pg_catalog.pg_depend"),
    ("pg_catalog.pg_description", "pg_catalog.pg_description"),
    ("pg_catalog.pg_index", "pg_catalog.pg_index"),
    ("pg_catalog.pg_inherits", "pg_catalog.pg_inherits"),
    ("pg_catalog.pg_language", "pg_catalog.pg_language"),
    ("pg_catalog.pg_namespace", "pg_catalog.pg_namespace"),
    ("pg_catalog.pg_rewrite", "pg_catalog.pg_rewrite"),
    ("pg_catalog.pg_trigger", "pg_catalog.pg_trigger"),
    ("pg_catalog.pg_db_role_setting", "pg_catalog.pg_db_role_setting"),
    ("pg_catalog.pg_partitioned_table", "pg_catalog.pg_partitioned_table"),
    ("pg_catalog.pg_collation", "pg_catalog.pg_collation"),
    ("pg_catalog.pg_range", "pg_catalog.pg_range"),
    ("pg_catalog.pg_roles", "pg_catalog.pg_authid,pg_catalog.pg_db_role_setting,pg_catalog.pg_roles"),
    ("pg_catalog.pg_shadow", "pg_catalog.pg_authid,pg_catalog.pg_db_role_setting,pg_catalog.pg_shadow"),
    ("pg_catalog.pg_user", "pg_catalog.pg_authid,pg_catalog.pg_db_role_setting,pg_catalog.pg_shadow,pg_catalog.pg_user"),
    ("pg_catalog.pg_rules", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_rewrite,pg_catalog.pg_rules"),
    ("pg_catalog.pg_views", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_views"),
    ("pg_catalog.pg_tables", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_tables,pg_catalog.pg_tablespace"),
    ("pg_catalog.pg_matviews", "pg_catalog.pg_class,pg_catalog.pg_matviews,pg_catalog.pg_namespace,pg_catalog.pg_tablespace"),
    ("pg_catalog.pg_indexes", "pg_catalog.pg_class,pg_catalog.pg_index,pg_catalog.pg_indexes,pg_catalog.pg_namespace,pg_catalog.pg_tablespace"),
    ("pg_catalog.pg_sequences", "pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_sequence,pg_catalog.pg_sequences"),
    ("pg_catalog.pg_prepared_statements", "pg_catalog.pg_prepared_statements"),
    ("pg_catalog.pg_settings", "pg_catalog.pg_settings"),
    ("information_schema.information_schema_catalog_name", "information_schema.information_schema_catalog_name"),
    ("information_schema.column_privileges", "information_schema.column_privileges,pg_catalog.pg_attribute,pg_catalog.pg_authid,pg_catalog.pg_class,pg_catalog.pg_namespace"),
    ("information_schema.columns", "information_schema.columns,pg_catalog.pg_attrdef,pg_catalog.pg_attribute,pg_catalog.pg_class,pg_catalog.pg_collation,pg_catalog.pg_depend,pg_catalog.pg_namespace,pg_catalog.pg_sequence,pg_catalog.pg_type"),
    ("information_schema.enabled_roles", "information_schema.enabled_roles,pg_catalog.pg_authid"),
    ("information_schema.key_column_usage", "information_schema.key_column_usage,pg_catalog.pg_attribute,pg_catalog.pg_class,pg_catalog.pg_constraint,pg_catalog.pg_namespace"),
    ("information_schema.role_column_grants", "information_schema.column_privileges,information_schema.enabled_roles,information_schema.role_column_grants,pg_catalog.pg_attribute,pg_catalog.pg_authid,pg_catalog.pg_class,pg_catalog.pg_namespace"),
    ("information_schema.routines", "information_schema.routines,pg_catalog.pg_language,pg_catalog.pg_namespace,pg_catalog.pg_proc,pg_catalog.pg_type"),
    ("information_schema.schemata", "information_schema.schemata,pg_catalog.pg_authid,pg_catalog.pg_namespace"),
    ("information_schema.sequences", "information_schema.sequences,pg_catalog.pg_class,pg_catalog.pg_depend,pg_catalog.pg_namespace,pg_catalog.pg_sequence"),
    ("information_schema.table_constraints", "information_schema.table_constraints,pg_catalog.pg_class,pg_catalog.pg_constraint,pg_catalog.pg_index,pg_catalog.pg_namespace"),
    ("information_schema.tables", "information_schema.tables,pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_type"),
    ("information_schema.views", "information_schema.views,pg_catalog.pg_class,pg_catalog.pg_namespace,pg_catalog.pg_trigger"),
 ];

fn assert_locks(first: &Engine, second: &Engine, expected: &str, mode: RelationLockMode) {
    for (name, _) in CATALOGS {
        let acquired = first
            .row_locks
            .try_acquire_relation(
                second.session_id,
                first.row_locks.table_key(name),
                mode,
                0,
                &second.runtime.cancellation,
            )
            .unwrap();
        first.row_locks.release_session(second.session_id);
        assert_eq!(
            !acquired,
            expected.split(',').any(|held| held == *name),
            "{name}, expected {expected}"
        );
    }
}

#[test]
fn system_query_and_explicit_locks_cover_the_postgresql_view_source_closure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        for (name, expected) in CATALOGS {
            let mut statements = vec![(
                format!("LOCK {name} IN SHARE MODE"),
                RelationLockMode::RowExclusive,
            )];
            if VirtualRelation::from_qualified_name(name).is_some() {
                statements.push((
                    format!("SELECT * FROM {name} LIMIT 0"),
                    RelationLockMode::AccessExclusive,
                ));
            }
            for (statement, probe) in statements {
                sql(&first, "BEGIN");
                sql(&first, &statement);
                assert_locks(&first, &second, expected, probe);
                sql(&first, "ROLLBACK");
                assert_locks(&first, &second, "", RelationLockMode::AccessExclusive);
            }
        }
    }
}

#[test]
fn system_lock_privileges_distinguish_public_views_from_private_role_sources() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; SET ROLE reader");
    for (name, _) in CATALOGS {
        for mode in [
            "ACCESS SHARE",
            "ROW SHARE",
            "ROW EXCLUSIVE",
            "SHARE UPDATE EXCLUSIVE",
            "SHARE",
            "SHARE ROW EXCLUSIVE",
            "EXCLUSIVE",
            "ACCESS EXCLUSIVE",
        ] {
            sql(&engine, "BEGIN");
            let query = format!("LOCK {name} IN {mode} MODE");
            if *name == "pg_catalog.pg_settings"
                || (mode == "ACCESS SHARE"
                    && !matches!(*name, "pg_catalog.pg_authid" | "pg_catalog.pg_shadow"))
            {
                sql(&engine, &query);
            } else {
                error(&engine, &query, "42501");
            }
            sql(&engine, "ROLLBACK");
        }
    }
}

#[test]
fn system_queries_wait_on_root_and_transitive_sources_until_savepoint_undo() {
    for provider in 0..3 {
        for (query, blocker) in [
            ("SELECT * FROM pg_class LIMIT 0", "pg_catalog.pg_class"),
            ("SELECT * FROM pg_user LIMIT 0", "pg_catalog.pg_shadow"),
            ("SELECT * FROM pg_user LIMIT 0", "pg_catalog.pg_authid"),
            (
                "SELECT * FROM information_schema.columns LIMIT 0",
                "pg_catalog.pg_collation",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                &format!("BEGIN; SAVEPOINT before_lock; LOCK {blocker} IN ACCESS EXCLUSIVE MODE"),
            );
            let (_, result) = after_wait(&first, second, query, blocker, "ROLLBACK TO before_lock");
            result.unwrap();
            sql(&first, "COMMIT");
        }
    }
}

#[test]
fn stored_views_and_cursors_retain_system_sources_and_respect_shadowing() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE VIEW catalog_users AS SELECT * FROM pg_user; CREATE TABLE public.pg_user(v integer); SET search_path=public,pg_catalog");
        sql(&first, "BEGIN; SELECT * FROM pg_user");
        assert_locks(&first, &second, "", RelationLockMode::AccessExclusive);
        sql(
            &first,
            "ROLLBACK; BEGIN; DECLARE catalog_cursor CURSOR FOR SELECT * FROM catalog_users",
        );
        let expected = "pg_catalog.pg_user,pg_catalog.pg_shadow,pg_catalog.pg_authid,pg_catalog.pg_db_role_setting";
        assert_locks(&first, &second, expected, RelationLockMode::AccessExclusive);
        sql(&first, "CLOSE catalog_cursor");
        assert_locks(&first, &second, expected, RelationLockMode::AccessExclusive);
        sql(&first, "COMMIT");
        sql(&first, "BEGIN; LOCK ONLY catalog_users IN SHARE MODE");
        assert_locks(&first, &second, expected, RelationLockMode::RowExclusive);
        sql(&second, "BEGIN");
        error(
            &second,
            "LOCK pg_catalog.pg_authid IN ACCESS EXCLUSIVE MODE NOWAIT",
            "55P03",
        );
        sql(&second, "ROLLBACK");
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn source_catalogs_use_the_same_regclass_identity_and_creation_collision_checks() {
    let engine = Engine::new();
    for (name, oid) in [
        ("pg_authid", 1260),
        ("pg_sequence", 2224),
        ("pg_shadow", 12005),
        ("information_schema.enabled_roles", 13410),
    ] {
        assert_eq!(
            sql(&engine, &format!("SELECT '{name}'::regclass::oid AS oid")).rows[0]["oid"],
            Value::Int(oid)
        );
        assert_eq!(
            sql(&engine, &format!("SELECT {oid}::regclass::text AS name")).rows[0]["name"],
            Value::Str(name.into())
        );
    }
    error(
        &engine,
        "CREATE TABLE pg_catalog.pg_authid(v integer)",
        "42P07",
    );
    sql(
        &engine,
        "CREATE TABLE public.pg_authid(v integer); SET search_path=public,pg_catalog",
    );
    assert_ne!(
        sql(&engine, "SELECT 'pg_authid'::regclass::oid AS oid").rows[0]["oid"],
        Value::Int(1260)
    );
}
