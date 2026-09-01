//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn pg18_role_membership_options_delegation_and_assumption_are_durable_security_state() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE membership_parent",
        "CREATE ROLE membership_member INHERIT",
        "CREATE ROLE membership_noinherit NOINHERIT",
        "CREATE ROLE membership_admin",
        "CREATE ROLE membership_delegate",
        "CREATE ROLE membership_no_set",
        "CREATE ROLE membership_middle INHERIT",
        "CREATE ROLE membership_leaf INHERIT",
        "CREATE ROLE membership_creator CREATEROLE",
        "CREATE ROLE membership_foreign",
        "CREATE ROLE membership_bootstrap SUPERUSER",
        "GRANT membership_parent TO membership_member, membership_noinherit",
        "GRANT membership_parent TO membership_admin WITH ADMIN OPTION, INHERIT FALSE, SET FALSE",
        "GRANT membership_parent TO membership_delegate GRANTED BY membership_admin",
        "GRANT membership_parent TO membership_middle",
        "GRANT membership_middle TO membership_leaf",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_membership_catalog_and_delegation(&engine);
    assert_membership_execution_and_inheritance(&engine);
    assert_membership_assumption_and_createrole(&engine);
}

fn assert_noinherit_member_cannot_manage_owned_routines(engine: &Engine) {
    assert_eq!(sqlstate(engine, "SELECT owner_execute_probe()"), "42501");
    assert_eq!(
        sqlstate(engine, "ALTER FUNCTION owner_alter_probe() IMMUTABLE"),
        "42501"
    );
    assert_eq!(
        sqlstate(
            engine,
            "CREATE OR REPLACE FUNCTION owner_replace_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 99'",
        ),
        "42501"
    );
    engine
        .sql(
            "GRANT EXECUTE ON FUNCTION owner_grant_probe() TO owner_acl_grantee",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "WARNING".into(),
            "no privileges were granted for \"owner_grant_probe\"".into(),
        )]
    );
    assert_eq!(
        sqlstate(engine, "DROP FUNCTION owner_drop_probe()"),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT owner_replace_probe() AS v"),
        Value::Int(13),
        "a rejected replacement must leave the original body intact"
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'owner_drop_probe'",
        ),
        Value::Int(1),
        "a rejected DROP must leave the routine intact"
    );
}

fn routine_acl_chain_engine() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE acl_delegate",
        "CREATE ROLE acl_member",
        "CREATE ROLE acl_leaf",
        "CREATE ROLE acl_tail",
        "CREATE ROLE acl_new_owner",
        "CREATE FUNCTION acl_chain_probe(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT value'",
        "REVOKE ALL ON FUNCTION acl_chain_probe(integer) FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_delegate WITH GRANT OPTION",
        "GRANT acl_delegate TO acl_member WITH INHERIT TRUE, SET TRUE",
        "SET ROLE acl_member",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'acl_chain_probe'",
        ),
        Value::Str("{uqa=X/uqa,acl_delegate=X*/uqa,acl_tail=X/acl_delegate}".into())
    );

    engine
}

fn extend_routine_acl_grantor_chain(engine: &Engine) {
    for sql in [
        "SET ROLE acl_delegate",
        "REVOKE EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_tail",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_leaf WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE acl_leaf",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail GRANTED BY CURRENT_USER",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            engine,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'acl_chain_probe'",
        ),
        Value::Str(
            "{uqa=X/uqa,acl_delegate=X*/uqa,acl_leaf=X*/acl_delegate,acl_tail=X/acl_leaf}".into(),
        )
    );
    assert_eq!(
        sqlstate(
            engine,
            "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail GRANTED BY acl_delegate",
        ),
        "0A000"
    );
    assert_eq!(
        sqlstate(
            engine,
            "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_delegate RESTRICT",
        ),
        "2BP01"
    );
}

fn cascade_routine_acl_without_removing_independent_path(engine: &Engine) {
    for sql in [
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail",
        "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_delegate CASCADE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            engine,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'acl_chain_probe'",
        ),
        Value::Str("{uqa=X/uqa,acl_delegate=X/uqa,acl_tail=X/uqa}".into())
    );
    engine.sql("SET ROLE acl_leaf", &[]).unwrap();
    assert_eq!(sqlstate(engine, "SELECT acl_chain_probe(18)"), "42501");
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("SET ROLE acl_tail", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT acl_chain_probe(18) AS v"),
        Value::Int(18)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn verify_routine_acl_alternate_paths_warnings_and_owner_transfer(engine: &Engine) {
    for sql in [
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_delegate WITH GRANT OPTION",
        "SET ROLE acl_delegate",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_leaf WITH GRANT OPTION",
        "RESET ROLE",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_leaf WITH GRANT OPTION",
        "REVOKE EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_tail",
        "SET ROLE acl_leaf",
        "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail",
        "RESET ROLE",
        "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_delegate CASCADE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(
            engine,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'acl_chain_probe'",
        ),
        Value::Str("{uqa=X/uqa,acl_delegate=X/uqa,acl_leaf=X*/uqa,acl_tail=X/acl_leaf}".into(),)
    );
    assert_eq!(
        sqlstate(
            engine,
            "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_leaf RESTRICT",
        ),
        "2BP01"
    );
    engine
        .sql(
            "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION acl_chain_probe(integer) FROM acl_leaf CASCADE",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE acl_leaf", &[]).unwrap();
    engine
        .sql(
            "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO acl_tail",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "WARNING".into(),
            "no privileges were granted for \"acl_chain_probe\"".into(),
        )]
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        sqlstate(
            engine,
            "GRANT EXECUTE ON FUNCTION acl_chain_probe(integer) TO PUBLIC WITH GRANT OPTION",
        ),
        "0LP01"
    );
    engine
        .sql(
            "ALTER FUNCTION acl_chain_probe(integer) OWNER TO acl_new_owner",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'acl_chain_probe'",
        ),
        Value::Str(
            "{acl_new_owner=X/acl_new_owner,acl_delegate=X/acl_new_owner,acl_leaf=X/acl_new_owner}"
                .into(),
        )
    );
}

#[test]
fn pg18_non_owner_routine_acl_grantor_chains_and_cascades_match_catalog_state() {
    let engine = routine_acl_chain_engine();
    extend_routine_acl_grantor_chain(&engine);
    cascade_routine_acl_without_removing_independent_path(&engine);
    verify_routine_acl_alternate_paths_warnings_and_owner_transfer(&engine);
}

#[test]
fn pg18_routine_acl_grantors_survive_reopen_and_transaction_rollback() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("routine-acl-grantors.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "CREATE ROLE durable_acl_delegate",
            "CREATE ROLE durable_acl_leaf",
            "CREATE FUNCTION durable_acl_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 18'",
            "REVOKE ALL ON FUNCTION durable_acl_probe() FROM PUBLIC",
            "GRANT EXECUTE ON FUNCTION durable_acl_probe() TO durable_acl_delegate WITH GRANT OPTION",
            "BEGIN",
            "SET ROLE durable_acl_delegate",
            "GRANT EXECUTE ON FUNCTION durable_acl_probe() TO durable_acl_leaf",
            "RESET ROLE",
            "ROLLBACK",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        engine.sql("SET ROLE durable_acl_leaf", &[]).unwrap();
        assert_eq!(sqlstate(&engine, "SELECT durable_acl_probe()"), "42501");
        engine.sql("RESET ROLE", &[]).unwrap();
        for sql in [
            "SET ROLE durable_acl_delegate",
            "GRANT EXECUTE ON FUNCTION durable_acl_probe() TO durable_acl_leaf",
            "RESET ROLE",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
    }

    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT proacl::text AS v FROM pg_catalog.pg_proc WHERE proname = 'durable_acl_probe'",
        ),
        Value::Str(
            "{uqa=X/uqa,durable_acl_delegate=X*/uqa,durable_acl_leaf=X/durable_acl_delegate}"
                .into(),
        )
    );
    reopened.sql("SET ROLE durable_acl_leaf", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT durable_acl_probe() AS v"),
        Value::Int(18)
    );
    reopened.sql("RESET ROLE", &[]).unwrap();
    for sql in [
        "BEGIN",
        "REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION durable_acl_probe() FROM durable_acl_delegate CASCADE",
        "ROLLBACK",
    ] {
        reopened
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    reopened.sql("SET ROLE durable_acl_leaf", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT durable_acl_probe() AS v"),
        Value::Int(18)
    );
}

fn assert_inherited_member_manages_owned_routines(engine: &Engine) {
    engine.sql("SET ROLE inherited_owner_member", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT owner_execute_probe() AS v"),
        Value::Int(11),
        "an inherited owner role retains its implicit EXECUTE privilege"
    );
    for sql in [
        "ALTER FUNCTION owner_alter_probe() IMMUTABLE",
        "CREATE OR REPLACE FUNCTION owner_replace_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 99'",
        "GRANT EXECUTE ON FUNCTION owner_grant_probe() TO owner_acl_grantee",
        "DROP FUNCTION owner_drop_probe()",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        sqlstate(
            engine,
            "ALTER FUNCTION owner_transfer_probe() OWNER TO owner_transfer_target",
        ),
        "42501",
        "the current owner must also be able to SET ROLE to the new owner"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT owner_transfer_target TO inherited_owner_member WITH INHERIT FALSE, SET TRUE",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE inherited_owner_member", &[]).unwrap();
    engine
        .sql(
            "ALTER FUNCTION owner_transfer_probe() OWNER TO owner_transfer_target",
            &[],
        )
        .unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn assert_owned_routine_changes_are_visible(engine: &Engine) {
    assert_eq!(
        scalar(engine, "SELECT owner_replace_probe() AS v"),
        Value::Int(99)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'owner_drop_probe'",
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT proc.provolatile AS v FROM pg_catalog.pg_proc AS proc WHERE proc.proname = 'owner_alter_probe'",
        ),
        Value::Str("i".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT owner.rolname AS v FROM pg_catalog.pg_proc AS proc JOIN pg_catalog.pg_roles AS owner ON owner.oid = proc.proowner WHERE proc.proname = 'owner_transfer_probe'",
        ),
        Value::Str("owner_transfer_target".into())
    );
    engine.sql("SET ROLE owner_acl_grantee", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT owner_grant_probe() AS v"),
        Value::Int(14)
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

#[test]
fn pg18_role_membership_governs_implicit_routine_ownership_and_owner_transfer() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE owned_routine_role",
        "CREATE ROLE inherited_owner_member",
        "CREATE ROLE noinherit_owner_member NOINHERIT",
        "CREATE ROLE owner_transfer_target",
        "CREATE ROLE owner_acl_grantee",
        "GRANT owned_routine_role TO inherited_owner_member",
        "GRANT owned_routine_role TO noinherit_owner_member WITH INHERIT FALSE, SET TRUE",
        "CREATE FUNCTION owner_execute_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 11'",
        "CREATE FUNCTION owner_alter_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 12'",
        "CREATE FUNCTION owner_replace_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 13'",
        "CREATE FUNCTION owner_grant_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 14'",
        "CREATE FUNCTION owner_drop_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 15'",
        "CREATE FUNCTION owner_transfer_probe() RETURNS integer LANGUAGE SQL AS 'SELECT 16'",
        "REVOKE ALL ON FUNCTION owner_execute_probe() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION owner_alter_probe() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION owner_replace_probe() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION owner_grant_probe() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION owner_drop_probe() FROM PUBLIC",
        "REVOKE ALL ON FUNCTION owner_transfer_probe() FROM PUBLIC",
        "ALTER FUNCTION owner_execute_probe() OWNER TO owned_routine_role",
        "ALTER FUNCTION owner_alter_probe() OWNER TO owned_routine_role",
        "ALTER FUNCTION owner_replace_probe() OWNER TO owned_routine_role",
        "ALTER FUNCTION owner_grant_probe() OWNER TO owned_routine_role",
        "ALTER FUNCTION owner_drop_probe() OWNER TO owned_routine_role",
        "ALTER FUNCTION owner_transfer_probe() OWNER TO owned_routine_role",
        "SET ROLE noinherit_owner_member",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_noinherit_member_cannot_manage_owned_routines(&engine);
    assert_inherited_member_manages_owned_routines(&engine);
    assert_owned_routine_changes_are_visible(&engine);
}

#[test]
fn pg18_roles_and_routine_security_survive_reopen_atomically() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("routine-security.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "BEGIN",
            "CREATE ROLE rolled_back_owner",
            "CREATE FUNCTION rolled_back_probe() RETURNS text LANGUAGE SQL AS 'SELECT current_user'",
            "ALTER FUNCTION rolled_back_probe() OWNER TO rolled_back_owner",
            "ROLLBACK",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_roles WHERE rolname = 'rolled_back_owner'",
            ),
            Value::Int(0)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'rolled_back_probe'",
            ),
            Value::Int(0)
        );
        for sql in [
            "CREATE ROLE persistent_owner",
            "CREATE ROLE persistent_caller LOGIN",
            "CREATE FUNCTION persistent_probe() RETURNS text LANGUAGE SQL SECURITY DEFINER AS 'SELECT current_user'",
            "ALTER FUNCTION persistent_probe() OWNER TO persistent_owner",
            "REVOKE ALL ON FUNCTION persistent_probe() FROM PUBLIC",
            "GRANT EXECUTE ON FUNCTION persistent_probe() TO persistent_caller",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert_eq!(
            sqlstate(&engine, "DROP ROLE persistent_owner"),
            "2BP01",
            "owned routines must prevent role removal"
        );
        assert_eq!(
            sqlstate(&engine, "DROP ROLE persistent_caller"),
            "2BP01",
            "routine ACLs must prevent role removal"
        );
    }
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_roles WHERE rolname = 'rolled_back_owner'",
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_proc WHERE proname = 'rolled_back_probe'",
        ),
        Value::Int(0)
    );
    reopened.sql("SET ROLE persistent_caller", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT persistent_probe() AS v"),
        Value::Str("persistent_owner".into())
    );
    let roles = reopened
        .sql(
            "SELECT rolname FROM pg_catalog.pg_roles WHERE rolname LIKE 'persistent_%' ORDER BY rolname",
            &[],
        )
        .unwrap();
    assert_eq!(roles.rows.len(), 2);
}

#[test]
fn pg18_role_memberships_survive_reopen_and_roll_back_with_the_role_catalog() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("role-memberships.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "CREATE ROLE durable_parent",
            "CREATE ROLE durable_member LOGIN",
            "CREATE ROLE durable_created_member",
            "CREATE ROLE durable_created_admin",
            "CREATE ROLE durable_created IN ROLE durable_parent ROLE durable_created_member ADMIN durable_created_admin",
            "GRANT durable_parent TO durable_member",
            "CREATE FUNCTION durable_membership_probe() RETURNS text LANGUAGE SQL AS 'SELECT current_user'",
            "REVOKE ALL ON FUNCTION durable_membership_probe() FROM PUBLIC",
            "GRANT EXECUTE ON FUNCTION durable_membership_probe() TO durable_parent",
            "BEGIN",
            "CREATE ROLE rolled_back_parent",
            "GRANT rolled_back_parent TO durable_member",
            "ROLLBACK",
        ] {
            engine
                .sql(sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_roles WHERE rolname = 'rolled_back_parent'",
            ),
            Value::Int(0)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid WHERE granted.rolname = 'rolled_back_parent'",
            ),
            Value::Int(0)
        );
    }

    let reopened = Engine::open(&path).unwrap();
    let initial = reopened
        .sql(
            "SELECT granted.rolname AS granted, member.rolname AS member, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname IN ('durable_parent', 'durable_created') ORDER BY granted.rolname, member.rolname",
            &[],
        )
        .unwrap();
    assert_eq!(initial.rows.len(), 4);
    assert!(initial
        .rows
        .iter()
        .any(|row| row["granted"] == Value::Str("durable_created".into())
            && row["member"] == Value::Str("durable_created_admin".into())
            && row["admin_option"] == Value::Bool(true)));
    reopened.sql("SET ROLE durable_member", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT durable_membership_probe() AS v"),
        Value::Str("durable_member".into())
    );
    reopened.sql("RESET ROLE", &[]).unwrap();
    reopened.sql("BEGIN", &[]).unwrap();
    reopened
        .sql("REVOKE durable_parent FROM durable_member", &[])
        .unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = 'durable_parent' AND member.rolname = 'durable_member'",
        ),
        Value::Int(0)
    );
    reopened.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = 'durable_parent' AND member.rolname = 'durable_member'",
        ),
        Value::Int(1)
    );
}
