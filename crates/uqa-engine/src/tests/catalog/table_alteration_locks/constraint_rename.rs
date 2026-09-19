//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named NOT NULL constraints preserve their own lifetime through rename and inheritance.

use super::{after_wait, error, peer_lock, sessions, sql, Engine, Value};

fn oid(engine: &Engine, table: &str, constraint: &str) -> Value {
    sql(engine, &format!("SELECT oid FROM pg_constraint WHERE conrelid='{table}'::regclass AND conname='{constraint}'")).rows[0]["oid"].clone()
}

#[test]
fn not_null_identity_survives_relation_column_and_constraint_renames() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
        let original = oid(&first, "t", "required");
        sql(
            &first,
            "ALTER TABLE t RENAME COLUMN v TO value; ALTER TABLE t RENAME TO renamed_table",
        );
        assert_eq!(oid(&first, "renamed_table", "required"), original);
        sql(
            &first,
            "ALTER TABLE renamed_table RENAME CONSTRAINT required TO renamed",
        );
        assert_eq!(oid(&first, "renamed_table", "renamed"), original);
        error(&first, "INSERT INTO renamed_table VALUES(NULL)", "23502");
    }
}

#[test]
fn recursive_not_null_rename_preserves_distinct_identities_and_savepoint_locks() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t)",
        );
        let parent = oid(&first, "t", "required");
        let child = oid(&first, "child", "required");
        assert_ne!(parent, child);
        sql(
            &first,
            "BEGIN; SAVEPOINT before_rename; ALTER TABLE t RENAME CONSTRAINT required TO renamed",
        );
        assert_eq!(oid(&first, "t", "renamed"), parent);
        assert_eq!(oid(&first, "child", "renamed"), child);
        peer_lock(&second, "child", "ACCESS SHARE", false);
        sql(&first, "ROLLBACK TO before_rename");
        assert_eq!(oid(&first, "t", "required"), parent);
        assert_eq!(oid(&first, "child", "required"), child);
        peer_lock(&second, "child", "ACCESS EXCLUSIVE", true);
        sql(&first, "COMMIT");
    }
}

#[test]
fn inherited_not_null_rename_uses_constraint_names_and_rejects_partial_trees() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t)",
        );
        error(
            &first,
            "ALTER TABLE ONLY t RENAME CONSTRAINT required TO renamed",
            "42P16",
        );
        error(
            &first,
            "ALTER TABLE child RENAME CONSTRAINT required TO renamed",
            "42P16",
        );
        sql(
            &first,
            "CREATE TABLE differently_named(v integer CONSTRAINT child_nn NOT NULL) INHERITS(t)",
        );
        error(
            &first,
            "ALTER TABLE t RENAME CONSTRAINT required TO renamed",
            "42704",
        );
        assert_eq!(
            sql(
                &first,
                "SELECT count(*) AS n FROM pg_constraint WHERE conname='required' AND contype='n'"
            )
            .rows[0]["n"],
            Value::Int(2)
        );
    }
}

#[test]
fn recreated_not_null_constraints_receive_a_new_identity() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
        let original = oid(&first, "t", "required");
        sql(&first, "ALTER TABLE t DROP CONSTRAINT required; ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
        assert_ne!(oid(&first, "t", "required"), original);
    }
}

#[test]
fn no_inherit_not_null_rename_changes_only_the_local_constraint() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v NO INHERIT; CREATE TABLE child() INHERITS(t)");
        let original = oid(&first, "t", "required");
        sql(
            &first,
            "BEGIN; ALTER TABLE ONLY t RENAME CONSTRAINT required TO renamed",
        );
        assert_eq!(oid(&first, "t", "renamed"), original);
        peer_lock(&second, "child", "ACCESS EXCLUSIVE", true);
        sql(&first, "COMMIT");
    }
}

#[test]
fn not_null_rename_follows_the_original_child_after_name_reuse() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t)",
        );
        let child = oid(&first, "child", "required");
        sql(&first, "BEGIN; ALTER TABLE child RENAME TO original; CREATE TABLE child(v integer CONSTRAINT required NOT NULL)");
        let replacement = oid(&first, "child", "required");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; ALTER TABLE t RENAME CONSTRAINT required TO renamed",
            "public.child",
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(oid(&second, "original", "renamed"), child);
        assert_eq!(oid(&second, "child", "required"), replacement);
        peer_lock(&first, "original", "ACCESS SHARE", false);
        peer_lock(&first, "child", "ACCESS EXCLUSIVE", true);
        sql(&second, "ROLLBACK");
        assert_eq!(oid(&second, "original", "required"), child);
    }
}

#[test]
fn renamed_not_null_identities_survive_persistent_reopen() {
    use std::sync::Arc;
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t)",
        );
        let parent = oid(&first, "t", "required");
        let child = oid(&first, "child", "required");
        sql(
            &first,
            "ALTER TABLE t RENAME CONSTRAINT required TO renamed",
        );
        drop(second);
        drop(first);
        let path = directory.path().join("table-locks.db");
        let reopened = match provider {
            0 => Engine::open(&path).unwrap(),
            1 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(oid(&reopened, "t", "renamed"), parent);
        assert_eq!(oid(&reopened, "child", "renamed"), child);
        error(&reopened, "INSERT INTO t VALUES(NULL)", "23502");
    }
}

#[test]
fn legacy_not_null_conversion_is_initial_only_and_rolls_back_on_later_restore_failure() {
    use std::sync::Arc;
    use uqa_execution::schema::constraints::restoration::IDENTITY_METADATA_KEY;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|schema| schema.relation.name == "t")
            .unwrap();
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&schema.columns_json).unwrap();
        columns[0].not_null_identity = None;
        schema.columns_json = serde_json::to_string(&columns).unwrap();
        raw.catalog.save_table(&schema).unwrap();
        raw.catalog.delete_metadata(IDENTITY_METADATA_KEY).unwrap();
        let Err(failure) = first.new_session() else {
            panic!("secondary session migrated NOT NULL identities")
        };
        assert!(
            failure
                .to_string()
                .contains("initial catalog identity migration"),
            "{failure}"
        );
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
                .columns_json,
            schema.columns_json
        );
        assert!(raw
            .catalog
            .get_metadata(IDENTITY_METADATA_KEY)
            .unwrap()
            .is_none());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        let legacy_oid = Value::Int(uqa_sql::catalog::oids::stable_oid(
            "constraint",
            "public.t.required",
        ));
        assert_eq!(oid(&restored, "t", "required"), legacy_oid);
        sql(
            &restored,
            "ALTER TABLE t RENAME CONSTRAINT required TO renamed",
        );
        assert_eq!(oid(&restored, "t", "renamed"), legacy_oid);
        error(&restored, "INSERT INTO t VALUES(NULL)", "23502");
        assert_eq!(
            raw.catalog
                .get_metadata(IDENTITY_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}

#[test]
fn current_not_null_identity_loss_is_rejected_without_repair() {
    use std::sync::Arc;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|schema| schema.relation.name == "t")
            .unwrap();
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&schema.columns_json).unwrap();
        columns[0].not_null_identity = None;
        schema.columns_json = serde_json::to_string(&columns).unwrap();
        raw.catalog.save_table(&schema).unwrap();
        assert!(first.new_session().is_err());
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(factory) else {
            panic!("missing current NOT NULL identity repaired")
        };
        assert!(
            failure
                .to_string()
                .contains("initial catalog identity migration"),
            "{failure}"
        );
        assert_eq!(
            raw.catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation.name == "t")
                .unwrap()
                .columns_json,
            schema.columns_json
        );
    }
}

#[test]
fn current_not_null_identity_duplicates_are_rejected_before_session_or_reload_publication() {
    use std::sync::Arc;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE duplicate(v integer NOT NULL)");
        let original = oid(&first, "t", "required");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schemas = raw.catalog.load_tables().unwrap();
        let source: Vec<uqa_sql::ast::ColumnDef> = serde_json::from_str(
            &schemas
                .iter()
                .find(|schema| schema.relation.name == "t")
                .unwrap()
                .columns_json,
        )
        .unwrap();
        let target = schemas
            .iter_mut()
            .find(|schema| schema.relation.name == "duplicate")
            .unwrap();
        let saved = target.clone();
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&target.columns_json).unwrap();
        columns[0].not_null_identity = source[0].not_null_identity;
        target.columns_json = serde_json::to_string(&columns).unwrap();
        raw.catalog.save_table(target).unwrap();
        let Err(failure) = first.new_session() else {
            panic!("duplicate NOT NULL identities loaded")
        };
        assert!(
            failure.to_string().contains("duplicate NOT NULL"),
            "{failure}"
        );
        let failure = first.reload_table_catalog_after_rollback().unwrap_err();
        assert!(
            failure.to_string().contains("duplicate NOT NULL"),
            "{failure}"
        );
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("duplicate NOT NULL identities migrated")
        };
        assert!(
            failure.to_string().contains("duplicate NOT NULL"),
            "{failure}"
        );
        raw.catalog.save_table(&saved).unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(oid(&restored, "t", "required"), original);
    }
}

#[test]
fn load_only_not_null_restore_requires_a_known_completed_conversion_marker() {
    use uqa_execution::schema::constraints::restoration::IDENTITY_METADATA_KEY;
    for provider in 0..3 {
        for version in [None, Some("2")] {
            let (_directory, first, _second) = sessions(provider);
            sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
            let raw = first
                .storage
                .provider
                .as_ref()
                .unwrap()
                .open_session()
                .unwrap();
            if let Some(version) = version {
                raw.catalog
                    .set_metadata(IDENTITY_METADATA_KEY, version)
                    .unwrap();
            } else {
                raw.catalog.delete_metadata(IDENTITY_METADATA_KEY).unwrap();
            }
            assert!(first.new_session().is_err());
            assert!(first.reload_table_catalog_after_rollback().is_err());
            assert_eq!(
                raw.catalog
                    .get_metadata(IDENTITY_METADATA_KEY)
                    .unwrap()
                    .as_deref(),
                version
            );
        }
    }
}
