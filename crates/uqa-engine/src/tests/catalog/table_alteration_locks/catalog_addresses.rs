//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{error, sessions, sql, Engine, Value};
use crate::tests::relation_lock_support::reopen;
use std::sync::Arc;

fn oid(engine: &Engine, table: &str, name: &str) -> Value {
    sql(
        engine,
        &format!(
            "SELECT oid FROM pg_constraint WHERE conrelid='{table}'::regclass AND conname='{name}'"
        ),
    )
    .rows[0]["oid"]
        .clone()
}

#[test]
fn key_and_check_addresses_survive_column_table_renames_and_persistent_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE original(v int CONSTRAINT positive CHECK(v>0), CONSTRAINT uk UNIQUE(v), CONSTRAINT upper_bound CHECK(v<10))");
        let before: Vec<_> = ["positive", "uk", "upper_bound"]
            .map(|name| oid(&first, "original", name))
            .into();
        sql(&first, "ALTER TABLE original RENAME COLUMN v TO value; ALTER TABLE original RENAME CONSTRAINT positive TO renamed_check; ALTER TABLE original RENAME TO renamed");
        for (name, expected) in ["renamed_check", "uk", "upper_bound"].iter().zip(&before) {
            assert_eq!(&oid(&first, "renamed", name), expected);
        }
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        for (name, expected) in ["renamed_check", "uk", "upper_bound"].iter().zip(&before) {
            assert_eq!(&oid(&restored, "renamed", name), expected);
        }
        sql(&restored, "INSERT INTO renamed VALUES(1)");
        error(&restored, "INSERT INTO renamed VALUES(1)", "23505");
        error(&restored, "INSERT INTO renamed VALUES(11)", "23514");
    }
}

#[test]
fn key_and_check_recreation_changes_only_the_removed_catalog_row() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT uk UNIQUE(v), ADD CONSTRAINT positive CHECK(v>0)",
        );
        let key = oid(&first, "t", "uk");
        let check = oid(&first, "t", "positive");
        sql(&first, "BEGIN; SAVEPOINT original; ALTER TABLE t DROP CONSTRAINT uk; ALTER TABLE t ADD CONSTRAINT uk UNIQUE(v)");
        assert_ne!(oid(&first, "t", "uk"), key);
        assert_eq!(oid(&first, "t", "positive"), check);
        sql(&first, "ROLLBACK TO original; COMMIT");
        assert_eq!(oid(&first, "t", "uk"), key);
        sql(&first, "ALTER TABLE t DROP CONSTRAINT positive; ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0)");
        assert_ne!(oid(&first, "t", "positive"), check);
        assert_eq!(oid(&first, "t", "uk"), key);
    }
}

fn attach(engine: &Engine) {
    sql(engine, "CREATE TABLE referenced(v int PRIMARY KEY); INSERT INTO referenced VALUES(1); CREATE TABLE p(v int UNIQUE REFERENCES referenced(v) DEFERRABLE) PARTITION BY RANGE(v); ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10)");
}

#[test]
fn detachment_preserves_local_key_and_foreign_key_oids_and_enforcement() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        attach(&first);
        let key = oid(&first, "t", "t_v_key");
        let reference = oid(&first, "t", "p_v_fkey");
        assert_ne!(key, oid(&first, "p", "p_v_key"));
        sql(
            &first,
            "ALTER TABLE p DETACH PARTITION t; ALTER TABLE p DROP CONSTRAINT p_v_fkey",
        );
        assert_eq!(oid(&first, "t", "t_v_key"), key);
        assert_eq!(oid(&first, "t", "p_v_fkey"), reference);
        error(&first, "INSERT INTO t VALUES(1)", "23505");
        error(&first, "INSERT INTO t VALUES(2)", "23503");
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(oid(&restored, "t", "t_v_key"), key);
        assert_eq!(oid(&restored, "t", "p_v_fkey"), reference);
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
    }
}

#[test]
fn detaching_a_subtree_separates_only_its_external_foreign_key_family() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(v int PRIMARY KEY); CREATE TABLE p(v int PRIMARY KEY REFERENCES referenced(v)) PARTITION BY RANGE(v); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10) PARTITION BY RANGE(v); CREATE TABLE leaf PARTITION OF c FOR VALUES FROM(0) TO(5)");
        let child = oid(&first, "c", "c_pkey");
        let leaf = oid(&first, "leaf", "leaf_pkey");
        sql(
            &first,
            "ALTER TABLE p DETACH PARTITION c; ALTER TABLE p DROP CONSTRAINT p_v_fkey",
        );
        assert_eq!(oid(&first, "c", "c_pkey"), child);
        assert_eq!(oid(&first, "leaf", "leaf_pkey"), leaf);
        error(&first, "INSERT INTO leaf VALUES(1)", "23503");
        sql(
            &first,
            "ALTER TABLE c DROP CONSTRAINT p_v_fkey; INSERT INTO leaf VALUES(1)",
        );
        error(&first, "INSERT INTO leaf VALUES(1)", "23505");
    }
}

#[test]
fn detach_preserves_both_constraint_modes_and_savepoint_undo() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        attach(&first);
        sql(
            &first,
            "CREATE TABLE other PARTITION OF p FOR VALUES FROM(10) TO(20)",
        );
        let reference = oid(&first, "t", "p_v_fkey");
        sql(&first, "BEGIN; SET CONSTRAINTS p_v_fkey DEFERRED; SAVEPOINT attached; ALTER TABLE p DETACH PARTITION t; INSERT INTO t VALUES(2); INSERT INTO p VALUES(12); INSERT INTO referenced VALUES(2),(12); ROLLBACK TO attached");
        assert_eq!(oid(&first, "t", "p_v_fkey"), reference);
        sql(&first, "ALTER TABLE p DETACH PARTITION t; INSERT INTO t VALUES(2); INSERT INTO p VALUES(12); INSERT INTO referenced VALUES(2),(12); COMMIT");
        assert_eq!(oid(&first, "t", "p_v_fkey"), reference);
    }
}

#[test]
fn current_key_or_check_identity_loss_is_rejected_without_repair() {
    for (provider, check) in
        (0..3).flat_map(|provider| [false, true].map(|check| (provider, check)))
    {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT uk UNIQUE(v), ADD CONSTRAINT positive CHECK(v>0)",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "t")
            .unwrap();
        let mut constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&schema.constraints_json).unwrap();
        if check {
            constraints.checks[0].catalog_oid = None;
        } else {
            constraints.key_constraints[0].catalog_identity = None;
        }
        schema.constraints_json = serde_json::to_string(&constraints).unwrap();
        raw.catalog.save_table(&schema).unwrap();
        assert!(first.new_session().is_err());
        assert!(first.reload_table_catalog_after_rollback().is_err());
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(factory) else {
            panic!("missing current constraint identity accepted")
        };
        assert!(
            failure.to_string().contains(if check {
                "CHECK constraints require"
            } else {
                "key constraints require"
            }),
            "{failure}"
        );
        assert_eq!(
            raw.catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation.name == "t")
                .unwrap()
                .constraints_json,
            schema.constraints_json
        );
    }
}

#[test]
fn legacy_key_and_check_conversion_rolls_back_with_a_later_restore_failure() {
    use uqa_execution::schema::constraints::restoration::CATALOG_ADDRESS_METADATA_KEY;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT uk UNIQUE(v), ADD CONSTRAINT positive CHECK(v>0)",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "t")
            .unwrap();
        let mut constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&schema.constraints_json).unwrap();
        let check_object = constraints.checks[0].object_id.unwrap();
        constraints.key_constraints[0].catalog_identity = None;
        constraints.checks[0].catalog_oid = None;
        schema.constraints_json = serde_json::to_string(&constraints).unwrap();
        raw.catalog.save_table(&schema).unwrap();
        raw.catalog
            .delete_metadata(CATALOG_ADDRESS_METADATA_KEY)
            .unwrap();
        crate::tests::catalog::hierarchy_restoration::legacy_index_registry(raw.catalog.as_ref());
        assert!(first.new_session().is_err());
        assert!(first.reload_table_catalog_after_rollback().is_err());
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("invalid trigger catalog accepted")
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert_eq!(
            raw.catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation.name == "t")
                .unwrap()
                .constraints_json,
            schema.constraints_json
        );
        assert!(raw
            .catalog
            .get_metadata(CATALOG_ADDRESS_METADATA_KEY)
            .unwrap()
            .is_none());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        let key_oid = Value::Int(uqa_sql::catalog::oids::stable_oid(
            "constraint",
            "public.t.uk",
        ));
        let check_oid = Value::Int(uqa_sql::catalog::oids::stable_object_oid(
            "constraint",
            &check_object,
        ));
        assert_eq!(oid(&restored, "t", "uk"), key_oid);
        assert_eq!(oid(&restored, "t", "positive"), check_oid);
        sql(
            &restored,
            "ALTER TABLE t RENAME COLUMN v TO value; ALTER TABLE t RENAME TO renamed",
        );
        assert_eq!(oid(&restored, "renamed", "uk"), key_oid);
        assert_eq!(oid(&restored, "renamed", "positive"), check_oid);
        error(&restored, "INSERT INTO renamed VALUES(1)", "23505");
        error(&restored, "INSERT INTO renamed VALUES(0)", "23514");
        assert_eq!(
            raw.catalog
                .get_metadata(CATALOG_ADDRESS_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}
