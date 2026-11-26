//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared role dependency waits preserve original identity and private transaction state.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{
        shared_objects::{SharedCatalogLock, SharedObjectLockSession},
        RelationLockMode,
    },
};

struct Target {
    setup: &'static str,
    grant: &'static str,
    access: Access,
}

enum Access {
    Inquiry(&'static str),
    Execute(&'static str),
}

const TARGETS: &[Target] = &[
    Target { setup: "", grant: "GRANT SELECT ON t TO dependent", access: Access::Inquiry("has_table_privilege('dependent', 't', 'SELECT')") },
    Target { setup: "", grant: "GRANT SELECT(v) ON t TO dependent", access: Access::Inquiry("has_column_privilege('dependent', 't', 'v', 'SELECT')") },
    Target { setup: "", grant: "GRANT UPDATE ON pg_class TO dependent WITH GRANT OPTION", access: Access::Inquiry("has_table_privilege('dependent', 'pg_class', 'UPDATE WITH GRANT OPTION')") },
    Target { setup: "", grant: "GRANT UPDATE(relname) ON pg_class TO dependent", access: Access::Inquiry("has_column_privilege('dependent', 'pg_class', 'relname', 'UPDATE')") },
    Target { setup: "CREATE VIEW role_view AS SELECT v FROM t", grant: "GRANT SELECT ON role_view TO dependent", access: Access::Inquiry("has_table_privilege('dependent', 'role_view', 'SELECT')") },
    Target { setup: "CREATE MATERIALIZED VIEW role_view AS SELECT v FROM t", grant: "GRANT SELECT ON role_view TO dependent", access: Access::Inquiry("has_table_privilege('dependent', 'role_view', 'SELECT')") },
    Target { setup: "CREATE SERVER role_remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE role_foreign(v integer) SERVER role_remote", grant: "GRANT SELECT ON role_foreign TO dependent", access: Access::Inquiry("has_table_privilege('dependent', 'role_foreign', 'SELECT')") },
    Target { setup: "", grant: "GRANT CREATE ON DATABASE uqa TO dependent", access: Access::Inquiry("has_database_privilege('dependent', 'uqa', 'CREATE')") },
    Target { setup: "CREATE SCHEMA role_schema", grant: "GRANT USAGE ON SCHEMA role_schema TO dependent", access: Access::Inquiry("has_schema_privilege('dependent', 'role_schema', 'USAGE')") },
    Target { setup: "CREATE SEQUENCE role_sequence", grant: "GRANT USAGE ON SEQUENCE role_sequence TO dependent", access: Access::Inquiry("has_sequence_privilege('dependent', 'role_sequence', 'USAGE')") },
    Target { setup: "CREATE SEQUENCE role_sequence", grant: "GRANT SELECT ON TABLE role_sequence TO dependent", access: Access::Inquiry("has_sequence_privilege('dependent', 'role_sequence', 'SELECT')") },
    Target { setup: "CREATE FUNCTION role_function() RETURNS integer LANGUAGE SQL AS 'SELECT 1'; REVOKE EXECUTE ON FUNCTION role_function() FROM PUBLIC", grant: "GRANT EXECUTE ON FUNCTION role_function() TO dependent", access: Access::Execute("SELECT role_function()") },
    Target { setup: "CREATE PROCEDURE role_procedure() LANGUAGE SQL AS 'SELECT 1'; REVOKE EXECUTE ON PROCEDURE role_procedure() FROM PUBLIC", grant: "GRANT EXECUTE ON PROCEDURE role_procedure() TO dependent", access: Access::Execute("CALL role_procedure()") },
];

fn role_lock(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()["dependent"].oid).unwrap(),
    }
}

fn role_exists(engine: &Engine) -> bool {
    sql(
        engine,
        "SELECT count(*) AS n FROM pg_roles WHERE rolname = 'dependent'",
    )
    .rows[0]["n"]
        == Value::Int(1)
}

fn allowed(engine: &Engine, target: &Target) -> bool {
    allowed_as(engine, target, "dependent")
}

fn allowed_as(engine: &Engine, target: &Target, role: &str) -> bool {
    match target.access {
        Access::Inquiry(inquiry) => {
            let inquiry = inquiry.replace("'dependent'", &format!("'{role}'"));
            sql(engine, &format!("SELECT {inquiry} AS allowed")).rows[0]["allowed"]
                == Value::Bool(true)
        }
        Access::Execute(statement) => {
            sql(engine, &format!("SET ROLE {role}"));
            let result = engine.sql(statement, &[]);
            sql(engine, "RESET ROLE");
            match result {
                Ok(_) => true,
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("42501"), "{statement}: {error}");
                    false
                }
            }
        }
    }
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn acl_waits_keep_the_original_recipient_after_rename_and_name_reuse(
    #[case] provider: usize,
    #[values("READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE")] isolation: &str,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)] target_index: usize,
) {
    let target = &TARGETS[target_index];
    let (directory, first, second) = sessions(provider);
    sql(&first, "CREATE ROLE dependent");
    if !target.setup.is_empty() {
        sql(&first, target.setup);
    }
    let original = first.durable.roles.read()["dependent"].identity();
    let lock = role_lock(&first);
    sql(&first, "BEGIN");
    first
        .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
        .unwrap()
        .retain();
    sql(
        &second,
        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
    );
    let (second, result) = after_wait(
        &first,
        second,
        target.grant,
        lock,
        "ALTER ROLE dependent RENAME TO renamed; CREATE ROLE dependent; COMMIT",
    );
    result.unwrap();
    sql(&second, "COMMIT");
    assert!(
        allowed_as(&first, target, "renamed"),
        "{provider}/{isolation}/{}",
        target.grant
    );
    assert!(
        !allowed(&first, target),
        "{provider}/{isolation}/{}",
        target.grant
    );
    assert_eq!(first.durable.roles.read()["renamed"].identity(), original);
    drop(second);
    drop(first);
    let restored = reopen(provider, &directory.path().join("table-locks.db"));
    assert!(allowed_as(&restored, target, "renamed"));
    assert!(!allowed(&restored, target));
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn role_drop_waits_for_added_acl_dependencies(
    #[case] provider: usize,
    #[values("READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE")] isolation: &str,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)] target_index: usize,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO before_grant; COMMIT")] finish: &str,
) {
    let target = &TARGETS[target_index];
    let (directory, first, second) = sessions(provider);
    sql(&first, "CREATE ROLE dependent");
    if !target.setup.is_empty() {
        sql(&first, target.setup);
    }
    let lock = role_lock(&first);
    sql(&first, "BEGIN; SAVEPOINT before_grant");
    sql(&first, target.grant);
    sql(
        &second,
        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
    );
    let (second, result) = after_wait(&first, second, "DROP ROLE dependent", lock, finish);
    if finish == "COMMIT" {
        assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
        sql(&second, "ROLLBACK");
        assert!(
            allowed(&first, target),
            "provider {provider}, {isolation}, {}, {finish}",
            target.grant
        );
    } else {
        result.unwrap();
        sql(&second, "COMMIT");
    }
    assert_eq!(role_exists(&first), finish == "COMMIT");
    drop(second);
    drop(first);
    let restored = reopen(provider, &directory.path().join("table-locks.db"));
    assert_eq!(role_exists(&restored), finish == "COMMIT");
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn acl_grants_wait_for_role_drop_and_never_rebind_a_recreated_name(
    #[case] provider: usize,
    #[values("READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE")] isolation: &str,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)] target_index: usize,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO before_drop; COMMIT", "RECREATE")] finish: &str,
) {
    let target = &TARGETS[target_index];
    let (directory, first, second) = sessions(provider);
    sql(&first, "CREATE ROLE dependent");
    if !target.setup.is_empty() {
        sql(&first, target.setup);
    }
    let lock = role_lock(&first);
    let original = first.durable.roles.read()["dependent"].object_id;
    sql(&first, "BEGIN; SAVEPOINT before_drop; DROP ROLE dependent");
    if finish == "RECREATE" {
        sql(&first, "CREATE ROLE dependent");
        assert_ne!(first.durable.roles.read()["dependent"].object_id, original);
    }
    sql(
        &second,
        &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
    );
    let (second, result) = after_wait(
        &first,
        second,
        target.grant,
        lock,
        if finish == "RECREATE" {
            "COMMIT"
        } else {
            finish
        },
    );
    let granted = finish != "COMMIT" && finish != "RECREATE";
    if granted {
        result.unwrap();
        sql(&second, "COMMIT");
        assert!(
            allowed(&first, target),
            "provider {provider}, {isolation}, {}, {finish}",
            target.grant
        );
    } else {
        assert_eq!(result.unwrap_err().sqlstate(), Some("42704"));
        sql(&second, "ROLLBACK");
        if finish == "RECREATE" {
            assert!(!allowed(&first, target));
        }
    }
    assert_eq!(
        sql(&first, "SELECT v FROM t").rows.len(),
        if granted { 2 } else { 1 }
    );
    drop(second);
    drop(first);
    let restored = reopen(provider, &directory.path().join("table-locks.db"));
    assert_eq!(role_exists(&restored), finish != "COMMIT");
    if finish != "COMMIT" {
        assert_eq!(allowed(&restored, target), granted);
    }
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn unchanged_acl_roles_and_revoke_do_not_acquire_new_dependency_locks(
    #[case] provider: usize,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)] target_index: usize,
) {
    let target = &TARGETS[target_index];
    let (_directory, first, second) = sessions(provider);
    sql(&first, "CREATE ROLE dependent");
    if !target.setup.is_empty() {
        sql(&first, target.setup);
    }
    sql(&first, target.grant);
    let revoke = target
        .grant
        .replacen("GRANT ", "REVOKE ", 1)
        .replace(" TO dependent", " FROM dependent")
        .replace(" WITH GRANT OPTION", "");
    let key = first.row_locks.shared_catalog_key(role_lock(&first));
    for statement in [target.grant, revoke.as_str()] {
        sql(&first, "BEGIN");
        sql(&first, statement);
        assert!(
            first
                .row_locks
                .try_acquire_relation(
                    second.session_id,
                    key,
                    RelationLockMode::AccessExclusive,
                    0,
                    &second.runtime.cancellation,
                )
                .unwrap(),
            "unexpected dependency lock for {statement}"
        );
        first.row_locks.release_session(second.session_id);
        sql(&first, "ROLLBACK");
    }
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn drop_owned_waits_for_the_role_identity_and_spares_its_reused_name(#[case] provider: usize) {
    let (_directory, holder, worker) = sessions(provider);
    let target = &TARGETS[0];
    sql(&holder, "CREATE ROLE dependent");
    if !target.setup.is_empty() {
        sql(&holder, target.setup);
    }
    sql(&holder, target.grant);
    let lock = role_lock(&holder);
    sql(&holder, "BEGIN");
    holder
        .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
        .unwrap()
        .retain();
    let release = format!(
        "ALTER ROLE dependent RENAME TO renamed; CREATE ROLE dependent; {}; COMMIT",
        target.grant
    );
    let (_worker, outcome) = after_wait(&holder, worker, "DROP OWNED BY dependent", lock, &release);
    outcome.unwrap();
    assert!(!allowed_as(&holder, target, "renamed"));
    assert!(allowed(&holder, target));
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn committed_temporary_default_acl_tracks_its_grantee_across_catalog_refresh(
    #[case] provider: usize,
) {
    let (_directory, first, second) = sessions(provider);
    sql(
        &first,
        "CREATE ROLE dependent; CREATE TEMP TABLE seed(i integer)",
    );
    let schema = first.temporary_schema_name();
    let grant =
        format!("ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} GRANT SELECT ON TABLES TO dependent");
    let revoke = format!(
        "ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} REVOKE SELECT ON TABLES FROM dependent"
    );
    sql(&first, &grant);
    assert_eq!(
        second
            .sql("DROP ROLE dependent", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    sql(&second, "CREATE SCHEMA refresh_default_acl");
    sql(&first, "CREATE TEMP TABLE after_refresh(i integer)");
    assert_eq!(
        sql(
            &first,
            "SELECT has_table_privilege('dependent','after_refresh','SELECT') AS permitted"
        )
        .rows[0]["permitted"],
        Value::Bool(true)
    );
    sql(&first, "DROP TABLE after_refresh");
    sql(&first, &format!("BEGIN; {revoke}; ROLLBACK"));
    assert_eq!(
        second
            .sql("DROP ROLE dependent", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    sql(&first, &revoke);
    sql(&second, "DROP ROLE dependent");
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn default_privilege_grantees_keep_the_original_role_after_a_wait(#[case] provider: usize) {
    let (_directory, first, second) = sessions(provider);
    sql(&first, "CREATE ROLE dependent");
    sql(&first, "BEGIN");
    let lock = role_lock(&first);
    first
        .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
        .unwrap()
        .retain();
    let (_, outcome) = after_wait(
        &first,
        second,
        "ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO dependent",
        lock,
        "ALTER ROLE dependent RENAME TO renamed; CREATE ROLE dependent; COMMIT",
    );
    outcome.unwrap();
    sql(&first, "CREATE TABLE after_wait(i integer)");
    let privileges = sql(&first, "SELECT has_table_privilege('renamed','after_wait','SELECT') AS original, has_table_privilege('dependent','after_wait','SELECT') AS replacement");
    assert_eq!(privileges.rows[0]["original"], Value::Bool(true));
    assert_eq!(privileges.rows[0]["replacement"], Value::Bool(false));
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn drop_owned_holds_the_original_type_oid_through_rename_and_name_reuse(#[case] provider: usize) {
    use uqa_execution::schema::namespaces::type_names::TYPE_CATALOG_CLASS_ID;
    let (_directory, holder, worker) = sessions(provider);
    sql(&holder, "CREATE ROLE dependent; CREATE DOMAIN original AS int; REVOKE ALL ON TYPE original FROM PUBLIC; GRANT USAGE ON TYPE original TO dependent");
    let oid = holder.durable.domains.read()["public.original"].oid;
    let lock = SharedCatalogLock::Object {
        class_id: TYPE_CATALOG_CLASS_ID,
        oid,
    };
    sql(&holder, "BEGIN");
    holder
        .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
        .unwrap()
        .retain();
    sql(&worker, "BEGIN");
    let (worker, result) = after_wait(
        &holder,
        worker,
        "DROP OWNED BY dependent",
        lock,
        "ALTER DOMAIN original RENAME TO moved; CREATE DOMAIN original AS int; COMMIT",
    );
    result.unwrap();
    let replacement = holder.durable.domains.read()["public.original"].oid;
    assert_ne!(oid, replacement);
    for (target_oid, available) in [(oid, false), (replacement, true)] {
        let key = holder
            .row_locks
            .shared_catalog_key(SharedCatalogLock::Object {
                class_id: TYPE_CATALOG_CLASS_ID,
                oid: target_oid,
            });
        assert_eq!(
            holder
                .row_locks
                .try_acquire_relation(
                    holder.session_id,
                    key,
                    RelationLockMode::AccessExclusive,
                    0,
                    &holder.runtime.cancellation
                )
                .unwrap(),
            available
        );
    }
    holder.row_locks.release_session(holder.session_id);
    sql(&worker, "COMMIT");
    assert_eq!(
        sql(
            &holder,
            "SELECT has_type_privilege('dependent', 'moved', 'USAGE') AS allowed"
        )
        .rows[0]["allowed"],
        Value::Bool(false)
    );
}
