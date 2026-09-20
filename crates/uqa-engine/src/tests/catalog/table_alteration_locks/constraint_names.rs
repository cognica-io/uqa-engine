//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint names are unique within their owning relation, including index-owned keys.

use super::{sessions, sql};
use crate::tests::relation_lock_support::{
    after_shared_wait, after_shared_wait_with_release, before_commit, reopen,
};
use uqa_core::RelationIdentity;
use uqa_execution::row_locks::shared_objects::SharedCatalogLock;

#[test]
fn check_rename_waits_for_an_uncommitted_owned_index_constraint_name() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0)");
        let owner_object_id =
            first.storage.tables.read()[&RelationIdentity::new("public", "t")].object_id();
        sql(&first, "BEGIN; ALTER INDEX one RENAME TO shared");
        let (_second, result) = after_shared_wait(
            &first,
            second,
            "ALTER TABLE t RENAME CONSTRAINT positive TO shared",
            SharedCatalogLock::MemberName {
                class_id: 2606,
                owner_class_id: 1259,
                owner_object_id,
                name: "shared",
            },
            "ROLLBACK",
        );
        result.unwrap();
    }
}

#[test]
fn constraint_names_coordinate_addition_and_renaming_in_both_orders() {
    for provider in 0..3 {
        for release in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
            for operation in [
                "ALTER TABLE t RENAME CONSTRAINT positive TO shared",
                "ALTER TABLE t RENAME CONSTRAINT required TO shared",
                "ALTER TABLE t RENAME CONSTRAINT reference TO shared",
                "ALTER TABLE t ADD CONSTRAINT shared CHECK(w<100)",
                "ALTER TABLE t ADD CONSTRAINT shared NOT NULL u",
                "ALTER TABLE t ADD CONSTRAINT shared FOREIGN KEY(f) REFERENCES parent(v)",
                "CREATE CONSTRAINT TRIGGER shared AFTER INSERT ON t DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fire()",
                "ALTER TABLE t RENAME CONSTRAINT event TO shared",
            ] {
                for reverse in [false, true] {
                    let (_directory, first, second) = sessions(provider);
                    sql(&first, "DROP TABLE t; CREATE TABLE parent(v int PRIMARY KEY); CREATE TABLE t(v int CONSTRAINT one UNIQUE,w int CONSTRAINT positive CHECK(w>0),n int CONSTRAINT required NOT NULL,f int CONSTRAINT reference REFERENCES parent(v),u int)");
                    sql(&first, "CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER event AFTER INSERT ON t DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fire()");
                    let object_id = first.storage.tables.read()
                        [&RelationIdentity::new("public", "t")]
                        .object_id();
                    let rename = "ALTER INDEX one RENAME TO shared";
                    let (holder, waiter) = if reverse {
                        (operation, rename)
                    } else {
                        (rename, operation)
                    };
                    sql(&first, &format!("BEGIN; SAVEPOINT undo; {holder}"));
                    let (_second, result) = after_shared_wait(
                        &first,
                        second,
                        waiter,
                        SharedCatalogLock::MemberName {
                            class_id: 2606,
                            owner_class_id: 1259,
                            owner_object_id: object_id,
                            name: "shared",
                        },
                        release,
                    );
                    if release == "COMMIT" {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("23505"),
                            "{provider}/{holder}/{waiter}"
                        );
                    } else {
                        result.unwrap_or_else(|error| {
                            panic!("{provider}/{holder}/{waiter}: {error}")
                        });
                    }
                }
            }
        }
    }
}

#[test]
fn constraint_name_wait_keeps_an_independently_renamed_key() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); ALTER TABLE t ADD CONSTRAINT two UNIQUE(v)");
        let third = first.new_session().unwrap();
        let object_id =
            first.storage.tables.read()[&RelationIdentity::new("public", "t")].object_id();
        sql(&first, "BEGIN; ALTER INDEX two RENAME TO held");
        let (second, result) = after_shared_wait_with_release(
            &first,
            second,
            "ALTER TABLE t ADD CONSTRAINT held CHECK(v<10)",
            SharedCatalogLock::MemberName {
                class_id: 2606,
                owner_class_id: 1259,
                owner_object_id: object_id,
                name: "held",
            },
            || {
                sql(&third, "ALTER INDEX one RENAME TO renamed");
                first.sql("ROLLBACK", &[])
            },
        );
        result.unwrap();
        assert!(second.catalog_index("renamed").unwrap().is_some());
        assert!(second.catalog_index("one").unwrap().is_none());
        drop(third);
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert!(restored.catalog_index("renamed").unwrap().is_some());
        assert!(restored.catalog_index("one").unwrap().is_none());
    }
}

#[test]
fn constraint_name_refresh_preserves_a_fixed_data_snapshot() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); CREATE TABLE payload(v int); INSERT INTO payload VALUES(1)");
            let third = first.new_session().unwrap();
            sql(&second, &format!("BEGIN ISOLATION LEVEL {isolation}"));
            sql(&second, "SELECT * FROM payload");
            let object_id =
                first.storage.tables.read()[&RelationIdentity::new("public", "t")].object_id();
            sql(&first, "BEGIN; ALTER INDEX one RENAME TO held");
            let (second, result) = after_shared_wait_with_release(
                &first,
                second,
                "ALTER TABLE t ADD CONSTRAINT held CHECK(v<10)",
                SharedCatalogLock::MemberName {
                    class_id: 2606,
                    owner_class_id: 1259,
                    owner_object_id: object_id,
                    name: "held",
                },
                || {
                    sql(&third, "INSERT INTO payload VALUES(2)");
                    first.sql("ROLLBACK", &[])
                },
            );
            result.unwrap();
            assert_eq!(sql(&second, "SELECT * FROM payload").rows.len(), 1);
            sql(&second, "COMMIT");
            assert_eq!(sql(&second, "SELECT * FROM payload").rows.len(), 2);
        }
    }
}

#[test]
fn index_name_wait_keeps_an_independently_renamed_key() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v)");
        let third = first.new_session().unwrap();
        sql(&first, "BEGIN; CREATE SEQUENCE held");
        let (second, result) = after_shared_wait_with_release(
            &first,
            second,
            "ALTER TABLE t ADD CONSTRAINT held UNIQUE(v)",
            SharedCatalogLock::Name {
                class_id: 1259,
                name: "public.held",
            },
            || {
                sql(&third, "ALTER INDEX one RENAME TO renamed");
                first.sql("ROLLBACK", &[])
            },
        );
        result.unwrap();
        assert!(second.catalog_index("renamed").unwrap().is_some());
        assert!(second.catalog_index("one").unwrap().is_none());
    }
}

#[test]
fn owned_index_name_wait_preserves_other_constraint_names() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); ALTER TABLE t ADD CONSTRAINT two UNIQUE(v); ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0)");
        let third = first.new_session().unwrap();
        let object_id =
            first.storage.tables.read()[&RelationIdentity::new("public", "t")].object_id();
        sql(
            &first,
            "BEGIN; ALTER TABLE t RENAME CONSTRAINT positive TO held",
        );
        let (second, result) = after_shared_wait_with_release(
            &first,
            second,
            "ALTER INDEX one RENAME TO held",
            SharedCatalogLock::MemberName {
                class_id: 2606,
                owner_class_id: 1259,
                owner_object_id: object_id,
                name: "held",
            },
            || {
                sql(&third, "ALTER INDEX two RENAME TO renamed");
                first.sql("ROLLBACK", &[])
            },
        );
        result.unwrap();
        let keys = second.try_key_constraints("t").unwrap();
        assert!(
            keys.iter()
                .any(|key| key.name.as_deref() == Some("renamed")),
            "{keys:?}"
        );
        assert!(
            keys.iter().any(|key| key.name.as_deref() == Some("held")),
            "{keys:?}"
        );
    }
}

#[test]
fn trigger_constraint_collision_diagnostics_preserve_source_precedence() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); ALTER TABLE t ADD CONSTRAINT positive CHECK(v>0); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER event AFTER INSERT ON t DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fire()");
        for (statement, state) in [
            ("CREATE CONSTRAINT TRIGGER positive AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION fire()", "23505"),
            ("CREATE CONSTRAINT TRIGGER event AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION fire()", "42710"),
            ("ALTER TABLE t RENAME CONSTRAINT positive TO event", "42710"),
            ("ALTER TABLE t ADD CONSTRAINT event CHECK(v<100)", "42710"),
            ("ALTER TABLE t ADD CONSTRAINT event NOT NULL v", "42710"),
            ("ALTER TABLE t ADD CONSTRAINT event UNIQUE(v)", "42710"),
            ("ALTER TABLE t ADD COLUMN w int CONSTRAINT event CHECK(w>0)", "42710"),
            ("ALTER TABLE t ADD COLUMN w int CONSTRAINT event NOT NULL", "42710"),
            ("ALTER TABLE t ADD COLUMN w int CONSTRAINT event UNIQUE", "42710"),
            ("ALTER INDEX one RENAME TO event", "42710"),
            ("ALTER TABLE t RENAME CONSTRAINT missing TO event", "42704"),
            ("ALTER TABLE t RENAME CONSTRAINT event TO positive", "42710"),
        ] {
            let failure = first.sql(statement, &[]).expect_err(statement);
            assert_eq!(failure.sqlstate(), Some(state), "{statement}: {failure}");
        }
        sql(&first, "ALTER TRIGGER event ON t RENAME TO positive");
    }
}

#[test]
fn automatic_column_constraint_names_skip_trigger_constraints() {
    for provider in 0..3 {
        for (definition, name) in [
            ("CHECK(w>0)", "t_w_check"),
            ("UNIQUE", "t_w_key"),
            ("NOT NULL", "t_w_not_null"),
        ] {
            let (_directory, first, _second) = sessions(provider);
            sql(&first, "DELETE FROM t; CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$");
            sql(&first, &format!("CREATE CONSTRAINT TRIGGER {name} AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION fire(); ALTER TABLE t ADD COLUMN w int {definition}"));
            let names = sql(
                &first,
                "SELECT conname FROM pg_constraint WHERE conrelid='t'::regclass ORDER BY conname",
            );
            assert_eq!(names.rows.len(), 2, "{provider}/{definition}: {names:?}");
            assert_eq!(names.rows[0]["conname"], super::Value::Str(name.into()));
            assert_eq!(
                names.rows[1]["conname"],
                super::Value::Str(format!("{name}1"))
            );
        }
    }
}

#[test]
fn partition_key_names_skip_local_trigger_constraints() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v int) PARTITION BY RANGE(v); ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER t_v_key AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION fire(); ALTER TABLE p ADD CONSTRAINT uk UNIQUE(v)");
        assert!(first.catalog_index("t_v_key1").unwrap().is_some());
        let names = sql(
            &first,
            "SELECT conname FROM pg_constraint WHERE conrelid='t'::regclass ORDER BY conname",
        );
        assert_eq!(names.rows.len(), 2);
        assert_eq!(
            names.rows[0]["conname"],
            super::Value::Str("t_v_key".into())
        );
        assert_eq!(
            names.rows[1]["conname"],
            super::Value::Str("t_v_key1".into())
        );
    }
}

#[test]
fn new_key_names_reserve_the_relation_before_the_constraint() {
    for provider in 0..3 {
        for release in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
            for operation in [
                "ALTER TABLE t ADD CONSTRAINT shared UNIQUE(v)",
                "ALTER TABLE t ADD CONSTRAINT shared PRIMARY KEY(v)",
                "ALTER TABLE t ADD COLUMN z int CONSTRAINT shared UNIQUE",
            ] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v)");
                sql(
                    &first,
                    "BEGIN; SAVEPOINT undo; ALTER INDEX one RENAME TO shared",
                );
                let (_second, result) = after_shared_wait(
                    &first,
                    second,
                    operation,
                    SharedCatalogLock::Name {
                        class_id: 1259,
                        name: "public.shared",
                    },
                    release,
                );
                if release == "COMMIT" {
                    let error = result.expect_err(operation);
                    assert_eq!(error.sqlstate(), Some("23505"), "{operation}: {error}");
                    assert!(
                        error.to_string().contains("pg_class_relname_nsp_index"),
                        "{operation}: {error}"
                    );
                } else {
                    result.unwrap_or_else(|error| panic!("{operation}: {error}"));
                }
            }
        }
    }
}

#[test]
fn added_column_preserves_named_unique_semantics_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD COLUMN w int CONSTRAINT \"Exact Key\" UNIQUE NULLS NOT DISTINCT; ALTER TABLE t ADD COLUMN IF NOT EXISTS w int CONSTRAINT ignored UNIQUE");
        assert!(first.catalog_index("\"Exact Key\"").unwrap().is_some());
        assert!(first.catalog_index("ignored").unwrap().is_none());
        super::error(&first, "INSERT INTO t(v) VALUES(2)", "23505");
        sql(&first, "INSERT INTO t(v,w) VALUES(2,2)");
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert!(restored.catalog_index("\"Exact Key\"").unwrap().is_some());
        super::error(&restored, "INSERT INTO t(v) VALUES(3)", "23505");
    }
}

#[test]
fn unrelated_index_and_other_owner_names_remain_independent() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX idx ON t(v); CREATE TABLE other(v int); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$");
        sql(&first, "BEGIN; ALTER INDEX idx RENAME TO shared");
        let second = before_commit(&first, second, "CREATE CONSTRAINT TRIGGER shared AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION fire(); ALTER TABLE other ADD CONSTRAINT shared CHECK(v>0)");
        drop(second);
    }
}
