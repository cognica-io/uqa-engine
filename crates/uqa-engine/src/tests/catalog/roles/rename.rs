//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL role names change without replacing authority or rewriting dependent catalogs.

use super::identity::reopen;
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;

mod coordination;

#[test]
fn sql_role_rename_enforces_error_order_authority_and_read_only_transactions() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE target LOGIN; CREATE ROLE actor CREATEROLE; CREATE ROLE plain; CREATE ROLE existing; CREATE ROLE elevated SUPERUSER; GRANT target TO actor WITH ADMIN OPTION");
    for (actor, from, to, expected) in [
        ("plain", "missing", "existing", "42704"),
        ("plain", "uqa", "existing", "0A000"),
        ("plain", "plain", "existing", "0A000"),
        ("plain", "target", "pg_reserved", "42939"),
        ("plain", "target", "existing", "42710"),
        ("plain", "target", "target", "42710"),
        ("plain", "target", "renamed", "42501"),
        ("actor", "existing", "renamed", "42501"),
        ("actor", "elevated", "renamed", "42501"),
    ] {
        sql(&engine, &format!("RESET ROLE; SET ROLE {actor}"));
        assert_eq!(
            engine
                .sql(&format!("ALTER ROLE {from} RENAME TO {to}"), &[])
                .unwrap_err()
                .sqlstate(),
            Some(expected),
            "{actor}/{from}/{to}"
        );
    }
    sql(&engine, "RESET ROLE; BEGIN READ ONLY");
    assert_eq!(
        engine
            .sql("ALTER ROLE target RENAME TO renamed", &[])
            .unwrap_err()
            .sqlstate(),
        Some("25006")
    );
    sql(
        &engine,
        "ROLLBACK; SET ROLE actor; ALTER GROUP target RENAME TO renamed",
    );
    assert_eq!(
        sql(
            &engine,
            "SELECT pg_has_role('actor', 'renamed', 'MEMBER WITH ADMIN OPTION') AS allowed"
        )
        .rows[0]["allowed"],
        Value::Bool(true)
    );
}

#[test]
fn private_role_name_moves_survive_peer_publication_savepoints_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE target");
                let original = first.durable.roles.read()["target"].identity();
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1; SAVEPOINT undo; ALTER ROLE target RENAME TO renamed; CREATE ROLE target"));
                sql(&second, "CREATE ROLE peer");
                sql(&first, "CREATE ROLE local_marker");
                assert_eq!(first.durable.roles.read()["renamed"].identity(), original);
                assert!(first.durable.roles.read().contains_key("peer"));
                sql(&first, finish);
                sql(&first, "SELECT rolname FROM pg_roles");
                let roles = first.durable.roles.read().clone();
                assert_eq!(roles.contains_key("renamed"), finish == "COMMIT");
                assert_eq!(roles["target"].identity() == original, finish != "COMMIT");
                assert!(roles.contains_key("peer"));
                drop(second);
                drop(first);
                assert_eq!(
                    *reopen(provider, &directory.path().join("table-locks.db"))
                        .durable
                        .roles
                        .read(),
                    roles
                );
            }
        }
    }
}

#[test]
fn sql_role_rename_preserves_durable_authority_and_reused_names() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE owner LOGIN CONNECTION LIMIT 7; CREATE ROLE reader; GRANT owner TO reader; CREATE SCHEMA owned AUTHORIZATION owner; SET ROLE owner; CREATE TABLE owned.items(id integer); INSERT INTO owned.items VALUES (1); CREATE VIEW owned.visible AS SELECT id FROM owned.items; CREATE MATERIALIZED VIEW owned.saved AS SELECT id FROM owned.items; CREATE SEQUENCE owned.counter; CREATE DOMAIN owned.positive AS integer CHECK (VALUE > 0); CREATE FUNCTION owned.answer() RETURNS text LANGUAGE sql SECURITY DEFINER AS 'SELECT current_user::text'; RESET ROLE; GRANT USAGE ON SCHEMA owned TO reader; GRANT SELECT ON owned.items, owned.visible, owned.saved TO reader; GRANT USAGE ON SEQUENCE owned.counter TO reader; REVOKE EXECUTE ON FUNCTION owned.answer() FROM PUBLIC; GRANT EXECUTE ON FUNCTION owned.answer() TO reader");
        let owner = first.durable.roles.read()["owner"].clone();
        let reader = first.durable.roles.read()["reader"].identity();
        sql(&second, "SET ROLE reader");
        sql(&first, "ALTER ROLE owner RENAME TO renamed_owner; ALTER USER reader RENAME TO renamed_reader; CREATE ROLE owner; CREATE ROLE reader");
        let roles = first.durable.roles.read();
        assert_eq!(roles["renamed_owner"].identity(), owner.identity());
        assert_eq!(roles["renamed_owner"].attributes, owner.attributes);
        assert_eq!(
            roles["renamed_owner"].connection_limit,
            owner.connection_limit
        );
        assert_eq!(roles["renamed_reader"].identity(), reader);
        assert_ne!(roles["reader"].identity(), reader);
        drop(roles);
        assert_authority(&first, owner.oid);
        let row = &sql(
            &second,
            "SELECT current_user AS who, owned.answer() AS answer",
        )
        .rows[0];
        assert_eq!(row["who"], Value::Str("renamed_reader".into()));
        assert_eq!(row["answer"], Value::Str("renamed_owner".into()));
        assert_eq!(
            sql(&second, "SELECT id FROM owned.visible").rows[0]["id"],
            Value::Int(1)
        );
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert_authority(&restored, owner.oid);
        assert_eq!(
            restored.durable.roles.read()["renamed_reader"].identity(),
            reader
        );
    }
}

fn assert_authority(engine: &Engine, owner_oid: i64) {
    let result = sql(engine, "SELECT relowner FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname='owned')");
    assert!(!result.rows.is_empty());
    for row in result.rows {
        assert_eq!(row["relowner"], Value::Int(owner_oid));
    }
    for query in [
        "SELECT proowner AS owner FROM pg_proc WHERE proname='answer'",
        "SELECT typowner AS owner FROM pg_type WHERE typname='positive'",
        "SELECT nspowner AS owner FROM pg_namespace WHERE nspname='owned'",
    ] {
        assert_eq!(sql(engine, query).rows[0]["owner"], Value::Int(owner_oid));
    }
    for (name, expected) in [("renamed_reader", true), ("reader", false)] {
        let row = &sql(engine, &format!("SELECT pg_has_role('{name}', 'renamed_owner', 'MEMBER') AS member, has_table_privilege('{name}', 'owned.items', 'SELECT') AS table_access, has_sequence_privilege('{name}', 'owned.counter', 'USAGE') AS sequence_access, has_function_privilege('{name}', 'owned.answer()', 'EXECUTE') AS routine_access")).rows[0];
        for field in [
            "member",
            "table_access",
            "sequence_access",
            "routine_access",
        ] {
            assert_eq!(row[field], Value::Bool(expected), "{name}/{field}");
        }
    }
}

#[test]
fn role_rename_uses_definer_authority_and_protects_the_outer_selected_user() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE caller; CREATE ROLE definer SUPERUSER; CREATE FUNCTION rename_definer() RETURNS text LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN ALTER ROLE definer RENAME TO renamed_definer; RETURN current_user; END $$; ALTER FUNCTION rename_definer() OWNER TO definer; CREATE FUNCTION rename_outer() RETURNS void LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN ALTER ROLE caller RENAME TO renamed_caller; END $$; ALTER FUNCTION rename_outer() OWNER TO definer; SET ROLE caller");
    assert_eq!(
        sql(&engine, "SELECT rename_definer() AS who").rows[0]["who"],
        Value::Str("renamed_definer".into())
    );
    assert_eq!(
        sql(&engine, "SELECT current_user AS who").rows[0]["who"],
        Value::Str("caller".into())
    );
    assert_eq!(
        engine
            .sql("SELECT rename_outer()", &[])
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
}

#[test]
fn bootstrap_role_rename_retains_database_owner_and_original_authentication() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE keeper SUPERUSER; SET SESSION AUTHORIZATION keeper; ALTER ROLE uqa RENAME TO renamed_bootstrap; CREATE ROLE uqa; RESET SESSION AUTHORIZATION");
        for engine in [&first, &second] {
            let row = &sql(engine, "SELECT current_user AS current_name, session_user AS session_name, (SELECT datdba FROM pg_database WHERE datname='uqa') AS owner").rows[0];
            assert_eq!(row["current_name"], Value::Str("renamed_bootstrap".into()));
            assert_eq!(row["session_name"], Value::Str("renamed_bootstrap".into()));
            assert_eq!(row["owner"], Value::Int(10));
        }
        let original = first.durable.roles.read()["renamed_bootstrap"].identity();
        assert_ne!(first.durable.roles.read()["uqa"].identity(), original);
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(
            sql(&restored, "SELECT current_user AS who").rows[0]["who"],
            Value::Str("renamed_bootstrap".into())
        );
        assert_eq!(
            restored.durable.roles.read()["renamed_bootstrap"].identity(),
            original
        );
    }
}
