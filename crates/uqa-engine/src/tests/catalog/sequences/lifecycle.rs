//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence renames and namespace moves retain and rebind their dependencies.

use crate::tests::relation_lock_support::{
    after_operation_wait, after_shared_wait, after_wait, error, reopen, sessions, sql,
};
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::{
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
    schema::namespaces::identity::SCHEMA_CATALOG_CLASS_ID,
};

fn schema_lock(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: SCHEMA_CATALOG_CLASS_ID,
        oid: engine.durable.schemas.read()["s"].tuple.unwrap().oid as u32,
    }
}

#[test]
fn sequence_schema_move_keeps_its_destination_alive_until_commit_or_undo() {
    for provider in 0..3 {
        for source in ["public.child", "s.child"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_move"] {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    &format!("CREATE SCHEMA s; CREATE SEQUENCE {source}"),
                );
                let target = schema_lock(&first);
                sql(
                    &first,
                    &format!("BEGIN; SAVEPOINT before_move; ALTER SEQUENCE {source} SET SCHEMA s"),
                );
                let (_, result) =
                    after_shared_wait(&first, second, "DROP SCHEMA s CASCADE", target, finish);
                result.unwrap_or_else(|error| panic!("{provider}/{source}/{finish}: {error}"));
                if finish.starts_with("ROLLBACK TO") {
                    sql(&first, "ROLLBACK");
                }
                let observer = first.new_session().unwrap();
                assert!(!observer.has_schema("s").unwrap());
                assert_eq!(
                    observer.sequence_state("public.child").unwrap().is_some(),
                    source == "public.child" && finish != "COMMIT"
                );
            }
        }
    }
}

#[test]
fn sequence_schema_move_rebinds_deleted_recreated_and_rolled_back_destinations() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for (holder, finish, missing) in [
                ("DROP SCHEMA s", "COMMIT", true),
                ("DROP SCHEMA s; CREATE SCHEMA s", "COMMIT", false),
                ("DROP SCHEMA s", "ROLLBACK", false),
                ("DROP SCHEMA s", "ROLLBACK TO before_drop", false),
            ] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE SCHEMA s; CREATE SEQUENCE public.child");
                let target = schema_lock(&first);
                sql(&first, &format!("BEGIN; SAVEPOINT before_drop; {holder}"));
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                let (second, result) = after_shared_wait(
                    &first,
                    second,
                    "ALTER SEQUENCE public.child SET SCHEMA s",
                    target,
                    finish,
                );
                if missing {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("3F000"));
                    sql(&second, "ROLLBACK");
                    assert!(second.sequence_state("public.child").unwrap().is_some());
                } else {
                    result.unwrap_or_else(|error| {
                        panic!("{provider}/{isolation}/{holder}/{finish}: {error}")
                    });
                    sql(&second, "COMMIT");
                    assert!(second.sequence_state("s.child").unwrap().is_some());
                }
                if finish.starts_with("ROLLBACK TO") {
                    sql(&first, "ROLLBACK");
                }
            }
        }
    }
}

#[test]
fn sequence_lifecycle_rechecks_source_names_kinds_and_definitions_after_waits() {
    for provider in 0..3 {
        for (action, target) in [
            ("RENAME TO renamed", "public.renamed"),
            ("SET SCHEMA s", "s.child"),
            ("INCREMENT BY 3", "public.child"),
            ("SET LOGGED", "public.child"),
        ] {
            for (holder, expected) in [
                ("ALTER SEQUENCE public.child RENAME TO moved", Some("42P01")),
                ("DROP SEQUENCE public.child", Some("42P01")),
                (
                    "DROP SEQUENCE public.child; CREATE TABLE public.child(id integer)",
                    Some("42809"),
                ),
                (
                    "DROP SEQUENCE public.child; CREATE SEQUENCE public.child START WITH 17",
                    None,
                ),
                ("ALTER SEQUENCE public.child INCREMENT BY 2", None),
            ] {
                eprintln!(
                    "sequence source wait: provider={provider}, holder={holder}, action={action}"
                );
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE SCHEMA s; CREATE SEQUENCE public.child");
                sql(&first, &format!("BEGIN; {holder}"));
                let (second, result) = after_wait(
                    &first,
                    second,
                    &format!("ALTER SEQUENCE public.child {action}"),
                    "public.child",
                    "COMMIT",
                );
                if let Some(state) = expected {
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.sqlstate(),
                        Some(state),
                        "{provider}/{action}/{holder}: {error}"
                    );
                } else {
                    result.unwrap_or_else(|error| panic!("{provider}/{action}/{holder}: {error}"));
                    let value = if holder.starts_with("DROP") { 17 } else { 1 };
                    let increment = if action == "INCREMENT BY 3" {
                        3
                    } else if value == 17 {
                        1
                    } else {
                        2
                    };
                    assert_eq!(second.nextval(target).unwrap(), value);
                    assert_eq!(second.nextval(target).unwrap(), value + increment);
                }
            }
        }
    }
}

#[test]
fn sequence_lifecycle_if_exists_skips_a_source_removed_during_the_wait() {
    for provider in 0..3 {
        for action in [
            "RENAME TO renamed",
            "SET SCHEMA s",
            "INCREMENT BY 3",
            "SET LOGGED",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SCHEMA s; CREATE SEQUENCE public.child");
            sql(&first, "BEGIN; ALTER SEQUENCE public.child RENAME TO moved");
            let (second, result) = after_wait(
                &first,
                second,
                &format!("ALTER SEQUENCE IF EXISTS public.child {action}"),
                "public.child",
                "COMMIT",
            );
            result.unwrap();
            assert_eq!(second.take_sql_notices().len(), 1);
            assert!(second.sequence_state("public.moved").unwrap().is_some());
        }
    }
}

#[test]
fn sequence_drop_waits_for_alterations_and_rebinds_the_requested_name() {
    for provider in 0..3 {
        for direct in [false, true] {
            for (holder, removed, moved) in [
                ("ALTER SEQUENCE public.child INCREMENT BY 2", true, false),
                ("ALTER SEQUENCE public.child RENAME TO moved", false, true),
                ("DROP SEQUENCE public.child", false, false),
                (
                    "DROP SEQUENCE public.child; CREATE SEQUENCE public.child START WITH 17",
                    true,
                    false,
                ),
            ] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE SEQUENCE public.child");
                sql(&first, &format!("BEGIN; {holder}"));
                let (second, result) =
                    after_operation_wait(&first, second, "public.child", "COMMIT", move |engine| {
                        if direct {
                            engine
                                .drop_sequence("public.child")
                                .map(Some)
                                .map_err(uqa_sql::SQLError::Internal)
                        } else {
                            engine.sql("DROP SEQUENCE IF EXISTS public.child", &[])?;
                            Ok(None)
                        }
                    });
                assert_eq!(
                    result.unwrap(),
                    direct.then_some(removed),
                    "{provider}/{direct}/{holder}"
                );
                assert!(second.sequence_state("public.child").unwrap().is_none());
                assert_eq!(
                    second.sequence_state("public.moved").unwrap().is_some(),
                    moved
                );
                assert_eq!(
                    second.take_sql_notices().len(),
                    usize::from(!direct && !removed)
                );
            }
        }
    }
}

#[test]
fn direct_sequence_drop_uses_a_memory_transaction_and_retains_sql_diagnostics() {
    let engine = Engine::new();
    assert!(!engine.drop_sequence("missing").unwrap());
    assert!(engine.take_sql_notices().is_empty());
    sql(&engine, "CREATE SEQUENCE child");
    assert!(engine.drop_sequence("child").unwrap());
    assert!(!engine.drop_sequence("child").unwrap());
    sql(&engine, "CREATE TABLE child(id integer)");
    assert!(engine
        .drop_sequence("child")
        .unwrap_err()
        .contains("not a sequence"));
    assert_eq!(engine.transaction_depth(), 0);
}

#[test]
fn sequence_schema_move_checks_collisions_against_the_replacement_namespace() {
    for provider in 0..3 {
        for replacement_collision in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE SCHEMA s; CREATE SEQUENCE public.child; CREATE SEQUENCE s.child",
            );
            let target = schema_lock(&first);
            sql(&first, "BEGIN; DROP SCHEMA s CASCADE; CREATE SCHEMA s");
            if replacement_collision {
                sql(&first, "CREATE TABLE s.child(id integer)");
            }
            let (second, result) = after_shared_wait(
                &first,
                second,
                "ALTER SEQUENCE public.child SET SCHEMA s",
                target,
                "COMMIT",
            );
            if replacement_collision {
                assert_eq!(result.unwrap_err().sqlstate(), Some("42P07"));
                assert!(second.sequence_state("public.child").unwrap().is_some());
            } else {
                result.unwrap();
                assert!(second.sequence_state("s.child").unwrap().is_some());
            }
        }
    }
}

#[test]
fn sequence_schema_move_rechecks_destination_create_after_namespace_replacement() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; GRANT CREATE ON SCHEMA public, s TO reader; CREATE SEQUENCE public.child; ALTER SEQUENCE public.child OWNER TO reader");
        let target = schema_lock(&first);
        sql(&second, "SET ROLE reader");
        sql(&first, "BEGIN; DROP SCHEMA s; CREATE SCHEMA s");
        let (second, result) = after_shared_wait(
            &first,
            second,
            "ALTER SEQUENCE public.child SET SCHEMA s",
            target,
            "COMMIT",
        );
        let error = result.unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"), "{provider}: {error}");
        assert!(second.sequence_state("public.child").unwrap().is_some());
    }
}

#[test]
fn sequence_lifecycle_rechecks_owner_authority_after_source_lock_waits() {
    for provider in 0..3 {
        for action in [
            "RENAME TO renamed",
            "SET SCHEMA s",
            "INCREMENT BY 3",
            "SET LOGGED",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other; CREATE SCHEMA s; GRANT CREATE ON SCHEMA public, s TO reader, other; CREATE SEQUENCE public.child; ALTER SEQUENCE public.child OWNER TO reader");
            sql(&second, "SET ROLE reader");
            sql(&first, "BEGIN; ALTER SEQUENCE public.child OWNER TO other");
            let (_, result) = after_wait(
                &first,
                second,
                &format!("ALTER SEQUENCE public.child {action}"),
                "public.child",
                "COMMIT",
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
        }
    }
}

#[test]
fn sequence_definition_locks_preserve_read_compatibility_and_savepoint_undo() {
    for provider in 0..3 {
        for (action, reads) in [
            ("INCREMENT BY 2", true),
            ("CACHE 1", true),
            ("OWNED BY NONE", true),
            ("SET LOGGED", false),
            ("SET UNLOGGED", false),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SEQUENCE public.child");
            sql(
                &first,
                &format!("BEGIN; SAVEPOINT before_alter; ALTER SEQUENCE public.child {action}"),
            );
            let available = |mode| {
                first
                    .row_locks
                    .try_acquire_scoped_relation(
                        second.session_id,
                        first.row_locks.table_key("public.child"),
                        mode,
                        (0, 1),
                        &second.runtime.cancellation,
                    )
                    .unwrap()
                    .is_some()
            };
            assert_eq!(
                available(RelationLockMode::AccessShare),
                reads,
                "{provider}/{action}"
            );
            assert!(
                !available(RelationLockMode::ShareRowExclusive),
                "{provider}/{action}"
            );
            sql(&first, "ROLLBACK TO before_alter");
            assert!(
                available(RelationLockMode::AccessExclusive),
                "{provider}/{action}"
            );
            sql(&first, "COMMIT");
        }
    }
}

#[test]
fn sequence_schema_move_preserves_identity_and_dependencies_through_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE SCHEMA s; CREATE SEQUENCE public.child; CREATE TABLE dependent(id bigint DEFAULT nextval('public.child')); CREATE VIEW dependent_view AS SELECT nextval('public.child') AS id");
        let before =
            first.durable.sequence_object_ids.read()[&RelationIdentity::new("public", "child")];
        sql(&second, "SET search_path = s; ALTER SEQUENCE public.child SET SCHEMA s; ALTER SEQUENCE s.child SET SCHEMA public");
        drop(second);
        drop(first);
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(
            engine.durable.sequence_object_ids.read()[&RelationIdentity::new("public", "child")],
            before
        );
        sql(&engine, "INSERT INTO dependent VALUES (DEFAULT)");
        assert_eq!(
            sql(&engine, "SELECT id FROM dependent").rows[0]["id"],
            uqa_core::Value::Int(1)
        );
        assert_eq!(
            sql(&engine, "SELECT id FROM dependent_view").rows[0]["id"],
            uqa_core::Value::Int(2)
        );
    }
}

#[test]
fn sequence_schema_move_orders_owned_and_temporary_errors_after_source_binding() {
    let engine = Engine::new();
    sql(&engine, "CREATE SCHEMA s; CREATE SEQUENCE child; CREATE TEMP SEQUENCE temporary_child; CREATE TEMP TABLE temporary_owner(id serial)");
    error(
        &engine,
        "ALTER SEQUENCE temporary_child SET SCHEMA missing",
        "3F000",
    );
    error(
        &engine,
        "ALTER SEQUENCE temporary_child SET SCHEMA s",
        "0A000",
    );
    let owned = engine
        .sql(
            "ALTER SEQUENCE temporary_owner_id_seq SET SCHEMA missing",
            &[],
        )
        .unwrap_err();
    assert_eq!(owned.sqlstate(), Some("0A000"));
    assert!(owned.to_string().contains("cannot move an owned sequence"));
    sql(&engine, "CREATE ROLE reader; GRANT CREATE ON SCHEMA public TO reader; ALTER SEQUENCE child OWNER TO reader; REVOKE TEMP ON DATABASE uqa FROM PUBLIC; SET ROLE reader");
    error(&engine, "ALTER SEQUENCE child SET SCHEMA pg_temp", "42501");
    sql(
        &engine,
        "RESET ROLE; GRANT TEMP ON DATABASE uqa TO reader; SET ROLE reader",
    );
    error(&engine, "ALTER SEQUENCE child SET SCHEMA pg_temp", "0A000");
}
