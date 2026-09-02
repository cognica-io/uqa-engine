//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[path = "security_roles/membership.rs"]
mod membership;
#[path = "security_roles/schema_usage.rs"]
mod schema_usage;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

#[test]
fn pg18_routine_owner_acl_context_and_catalog_move_together() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_owner",
        "CREATE ROLE routine_caller LOGIN",
        "CREATE FUNCTION secured_probe() RETURNS text LANGUAGE SQL SECURITY DEFINER LEAKPROOF PARALLEL SAFE SUPPORT textlike_support SET search_path TO pg_catalog AS 'SELECT current_user || ''/'' || session_user || ''/'' || current_schema'",
        "ALTER FUNCTION secured_probe() OWNER TO routine_owner",
        "REVOKE ALL ON FUNCTION secured_probe() FROM PUBLIC",
        "CREATE FUNCTION strict_secured(value integer) RETURNS integer LANGUAGE SQL STRICT AS 'SELECT value'",
        "REVOKE ALL ON FUNCTION strict_secured(integer) FROM PUBLIC",
        "SET ROLE routine_caller",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_eq!(sqlstate(&engine, "SELECT secured_probe()"), "42501");
    assert_eq!(
        sqlstate(&engine, "SELECT strict_secured(NULL)"),
        "42501",
        "STRICT must not bypass EXECUTE privilege checks"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT EXECUTE ON FUNCTION secured_probe() TO routine_caller WITH GRANT OPTION",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_caller", &[]).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT secured_probe() AS v"),
        Value::Str("routine_owner/uqa/pg_catalog".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT current_user AS v"),
        Value::Str("routine_caller".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT session_user AS v"),
        Value::Str("uqa".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT current_schema AS v"),
        Value::Str("public".into()),
        "routine SET must be restored after return"
    );

    let proc_row = engine
        .sql(
            "SELECT proowner, prosecdef, proleakproof, proparallel, prosupport, proconfig, proacl FROM pg_catalog.pg_proc WHERE proname = 'secured_probe'",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap();
    let owner_oid = engine
        .sql(
            "SELECT oid AS v FROM pg_catalog.pg_roles WHERE rolname = 'routine_owner'",
            &[],
        )
        .unwrap()
        .rows[0]["v"]
        .clone();
    assert_eq!(proc_row["proowner"], owner_oid);
    assert_eq!(proc_row["prosecdef"], Value::Bool(true));
    assert_eq!(proc_row["proleakproof"], Value::Bool(true));
    assert_eq!(proc_row["proparallel"], Value::Str("s".into()));
    assert_eq!(proc_row["prosupport"], Value::Int(1023));
    assert_ne!(proc_row["proconfig"], Value::Null);
    assert_ne!(proc_row["proacl"], Value::Null);

    engine
        .register_scalar_function("registered_supportless_probe", |_args: &[Value]| {
            Ok(Value::Null)
        })
        .unwrap();
    let support_values = engine
        .sql(
            "SELECT prosupport FROM pg_catalog.pg_proc WHERE proname IN ('random', 'registered_supportless_probe', 'secured_probe', 'strict_secured')",
            &[],
        )
        .unwrap();
    assert!(!support_values.rows.is_empty());
    assert!(support_values
        .rows
        .iter()
        .all(|row| matches!(row["prosupport"], Value::Int(_))));
    assert!(support_values
        .rows
        .iter()
        .any(|row| row["prosupport"] == Value::Int(0)));
    assert!(support_values
        .rows
        .iter()
        .any(|row| row["prosupport"] == Value::Int(1023)));
}

#[test]
fn pg18_support_function_validation_precedes_superuser_check() {
    let engine = Engine::new();
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FUNCTION missing_support_probe() RETURNS integer LANGUAGE SQL SUPPORT missing_support AS 'SELECT 1'",
        ),
        "42883"
    );
    engine.sql("CREATE ROLE support_caller LOGIN", &[]).unwrap();
    engine
        .sql("GRANT CREATE ON SCHEMA public TO support_caller", &[])
        .unwrap();
    engine.sql("SET ROLE support_caller", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE FUNCTION forbidden_support_probe() RETURNS integer LANGUAGE SQL SUPPORT textlike_support AS 'SELECT 1'",
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    let rows = engine
        .sql(
            "SELECT proname FROM pg_catalog.pg_proc WHERE proname IN ('missing_support_probe', 'forbidden_support_probe')",
            &[],
        )
        .unwrap();
    assert!(rows.rows.is_empty());
}

#[test]
fn pg18_alter_security_configuration_and_planner_metadata_are_atomic() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE attribute_owner",
        "CREATE ROLE attribute_caller LOGIN",
        "CREATE FUNCTION attribute_probe() RETURNS text LANGUAGE SQL SECURITY INVOKER PARALLEL UNSAFE AS 'SELECT current_user || ''/'' || current_schema'",
        "ALTER FUNCTION attribute_probe() OWNER TO attribute_owner",
        "ALTER FUNCTION attribute_probe() SECURITY DEFINER LEAKPROOF PARALLEL RESTRICTED SUPPORT numeric_support SET search_path TO pg_catalog",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(&engine, "SELECT attribute_probe() AS v"),
        Value::Str("attribute_owner/pg_catalog".into())
    );
    let before = engine
        .sql(
            "SELECT prosecdef, proleakproof, proparallel, prosupport, proconfig FROM pg_catalog.pg_proc WHERE proname = 'attribute_probe'",
            &[],
        )
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(before["prosecdef"], Value::Bool(true));
    assert_eq!(before["proleakproof"], Value::Bool(true));
    assert_eq!(before["proparallel"], Value::Str("r".into()));
    assert_eq!(before["prosupport"], Value::Int(3157));
    assert_ne!(before["proconfig"], Value::Null);

    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FUNCTION attribute_probe() SUPPORT missing_support",
        ),
        "42883"
    );
    assert_eq!(
        sqlstate(&engine, "ALTER FUNCTION attribute_probe() SUPPORT NONE"),
        "42883",
        "PostgreSQL 18 resolves NONE as a support-function name"
    );
    engine.sql("SET ROLE attribute_caller", &[]).unwrap();
    assert_eq!(
        sqlstate(&engine, "ALTER FUNCTION attribute_probe() SECURITY INVOKER",),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    let after = engine
        .sql(
            "SELECT prosecdef, proleakproof, proparallel, prosupport, proconfig FROM pg_catalog.pg_proc WHERE proname = 'attribute_probe'",
            &[],
        )
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(after, before);

    engine
        .sql(
            "ALTER FUNCTION attribute_probe() SET search_path TO DEFAULT",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT attribute_probe() AS v"),
        Value::Str("attribute_owner/public".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT proconfig IS NULL AS v FROM pg_catalog.pg_proc WHERE proname = 'attribute_probe'",
        ),
        Value::Bool(true)
    );
}

#[test]
fn routine_context_restores_identity_and_settings_after_callback_panic() {
    let engine = Engine::new();
    engine
        .register_scalar_function(
            "routine_context_panic",
            |_args: &[Value]| -> Result<Value, uqa_sql::SQLError> {
                panic!("routine callback panic")
            },
        )
        .unwrap();
    for sql in [
        "CREATE ROLE panic_owner",
        "CREATE ROLE panic_caller LOGIN",
        "CREATE FUNCTION panic_probe() RETURNS integer LANGUAGE SQL SECURITY DEFINER SET search_path TO pg_catalog AS 'SELECT routine_context_panic()'",
        "ALTER FUNCTION panic_probe() OWNER TO panic_owner",
        "SET ROLE panic_caller",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = engine.sql("SELECT panic_probe()", &[]);
    }));
    assert!(panic.is_err());
    assert_eq!(
        scalar(&engine, "SELECT current_user AS v"),
        Value::Str("panic_caller".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT current_schema AS v"),
        Value::Str("public".into())
    );
}

#[test]
fn pg18_invoker_set_role_persists_while_security_definer_and_errors_restore_identity() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_role_target",
        "CREATE ROLE routine_definer_owner",
        "CREATE FUNCTION invoker_set_role_probe() RETURNS text LANGUAGE plpgsql SECURITY INVOKER AS $$ BEGIN EXECUTE 'SET ROLE routine_role_target'; RETURN current_user; END $$",
        "CREATE FUNCTION failed_invoker_set_role_probe() RETURNS text LANGUAGE plpgsql SECURITY INVOKER AS $$ BEGIN EXECUTE 'SET ROLE routine_role_target'; RAISE EXCEPTION 'stop'; END $$",
        "CREATE FUNCTION definer_set_role_probe() RETURNS text LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN EXECUTE 'SET ROLE routine_role_target'; RETURN current_user; END $$",
        "ALTER FUNCTION definer_set_role_probe() OWNER TO routine_definer_owner",
        "CREATE FUNCTION nested_invoker_set_role_probe() RETURNS text LANGUAGE SQL SECURITY INVOKER AS 'SELECT invoker_set_role_probe()'",
        "CREATE FUNCTION definer_calls_invoker_probe() RETURNS text LANGUAGE SQL SECURITY DEFINER AS 'SELECT nested_invoker_set_role_probe()'",
        "ALTER FUNCTION definer_calls_invoker_probe() OWNER TO routine_definer_owner",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    assert_eq!(
        scalar(&engine, "SELECT invoker_set_role_probe() AS v"),
        Value::Str("routine_role_target".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT current_user AS v"),
        Value::Str("routine_role_target".into()),
        "successful SECURITY INVOKER SET ROLE must remain visible to the session"
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    assert_eq!(
        sqlstate(&engine, "SELECT failed_invoker_set_role_probe()"),
        "P0001"
    );
    assert_eq!(
        scalar(&engine, "SELECT current_user AS v"),
        Value::Str("uqa".into()),
        "a failing statement must roll back the invoker's role change"
    );
    assert_eq!(
        sqlstate(&engine, "SELECT definer_set_role_probe()"),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "SELECT definer_calls_invoker_probe()"),
        "42501",
        "a nested invoker must remain inside the outer security-definer restriction"
    );
    assert_eq!(
        scalar(&engine, "SELECT current_user AS v"),
        Value::Str("uqa".into())
    );
}

#[test]
fn pg18_role_lifecycle_updates_session_privileges_and_catalogs_atomically() {
    let engine = Engine::new();
    engine
        .sql("CREATE USER catalog_user CREATEDB CONNECTION LIMIT 4", &[])
        .unwrap();
    let user = engine
        .sql(
            "SELECT usecreatedb, usesuper, userepl, usebypassrls FROM pg_catalog.pg_user WHERE usename = 'catalog_user'",
            &[],
        )
        .unwrap();
    assert_eq!(user.rows.len(), 1);
    assert_eq!(user.rows[0]["usecreatedb"], Value::Bool(true));
    assert_eq!(user.rows[0]["usesuper"], Value::Bool(false));
    assert_eq!(sqlstate(&engine, "CREATE ROLE catalog_user"), "42710");

    engine
        .sql(
            "ALTER ROLE catalog_user NOLOGIN NOCREATEDB NOCREATEROLE REPLICATION BYPASSRLS CONNECTION LIMIT 7",
            &[],
        )
        .unwrap();
    let role = engine
        .sql(
            "SELECT rolcreaterole, rolcreatedb, rolcanlogin, rolreplication, rolconnlimit, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = 'catalog_user'",
            &[],
        )
        .unwrap();
    assert_eq!(role.rows.len(), 1);
    assert_eq!(role.rows[0]["rolcreaterole"], Value::Bool(false));
    assert_eq!(role.rows[0]["rolcreatedb"], Value::Bool(false));
    assert_eq!(role.rows[0]["rolcanlogin"], Value::Bool(false));
    assert_eq!(role.rows[0]["rolreplication"], Value::Bool(true));
    assert_eq!(role.rows[0]["rolconnlimit"], Value::Int(7));
    assert_eq!(role.rows[0]["rolbypassrls"], Value::Bool(true));
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_user WHERE usename = 'catalog_user'",
        ),
        Value::Int(0)
    );

    engine.sql("SET ROLE catalog_user", &[]).unwrap();
    assert_eq!(sqlstate(&engine, "CREATE ROLE forbidden_child"), "42501");
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(sqlstate(&engine, "ALTER ROLE missing_role LOGIN"), "42704");
    assert_eq!(sqlstate(&engine, "DROP ROLE missing_role"), "42704");
    engine.sql("DROP ROLE IF EXISTS missing_role", &[]).unwrap();
    assert_eq!(engine.take_sql_notices().len(), 1);
    engine.sql("DROP ROLE catalog_user", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_roles WHERE rolname = 'catalog_user'",
        ),
        Value::Int(0)
    );
}

#[test]
fn pg18_createrole_cannot_delegate_role_attributes_it_does_not_hold() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE limited_role_creator CREATEROLE",
        "CREATE ROLE full_role_creator CREATEROLE CREATEDB REPLICATION BYPASSRLS",
        "SET ROLE limited_role_creator",
        "CREATE ROLE limited_managed_role CREATEROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for sql in [
        "CREATE ROLE forbidden_createdb_role CREATEDB",
        "CREATE ROLE forbidden_replication_role REPLICATION",
        "CREATE ROLE forbidden_bypassrls_role BYPASSRLS",
        "ALTER ROLE limited_managed_role CREATEDB",
        "ALTER ROLE limited_managed_role REPLICATION",
        "ALTER ROLE limited_managed_role BYPASSRLS",
    ] {
        assert_eq!(sqlstate(&engine, sql), "42501", "{sql}");
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_roles WHERE rolname IN ('forbidden_createdb_role', 'forbidden_replication_role', 'forbidden_bypassrls_role')",
        ),
        Value::Int(0),
        "failed CREATE ROLE statements must not publish catalog rows"
    );
    let limited = engine
        .sql(
            "SELECT rolcreaterole, rolcreatedb, rolreplication, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = 'limited_managed_role'",
            &[],
        )
        .unwrap();
    assert_eq!(limited.rows.len(), 1);
    assert_eq!(limited.rows[0]["rolcreaterole"], Value::Bool(true));
    assert_eq!(limited.rows[0]["rolcreatedb"], Value::Bool(false));
    assert_eq!(limited.rows[0]["rolreplication"], Value::Bool(false));
    assert_eq!(limited.rows[0]["rolbypassrls"], Value::Bool(false));

    engine.sql("SET ROLE full_role_creator", &[]).unwrap();
    engine
        .sql(
            "CREATE ROLE fully_delegated_role CREATEROLE CREATEDB REPLICATION BYPASSRLS",
            &[],
        )
        .unwrap();
    let delegated = engine
        .sql(
            "SELECT rolcreaterole, rolcreatedb, rolreplication, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = 'fully_delegated_role'",
            &[],
        )
        .unwrap();
    assert_eq!(delegated.rows.len(), 1);
    assert!(delegated.rows[0]
        .values()
        .all(|value| *value == Value::Bool(true)));
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn create_pg_has_role_graph(engine: &Engine) {
    for sql in [
        "CREATE ROLE has_role_parent",
        "CREATE ROLE has_role_middle",
        "CREATE ROLE has_role_leaf",
        "CREATE ROLE has_role_noinherit NOINHERIT",
        "CREATE ROLE has_role_admin",
        "GRANT has_role_parent TO has_role_middle WITH ADMIN FALSE, INHERIT TRUE, SET FALSE",
        "GRANT has_role_middle TO has_role_leaf WITH ADMIN FALSE, INHERIT TRUE, SET TRUE",
        "GRANT has_role_parent TO has_role_noinherit WITH ADMIN FALSE, INHERIT FALSE, SET TRUE",
        "GRANT has_role_parent TO has_role_admin WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn pg18_pg_has_role_distinguishes_member_usage_set_and_admin_privileges() {
    let engine = Engine::new();
    create_pg_has_role_graph(&engine);

    let row = engine
        .sql(
            "SELECT pg_has_role('has_role_leaf', 'has_role_parent', 'MEMBER') AS leaf_member, pg_has_role('has_role_leaf', 'has_role_parent', 'USAGE') AS leaf_usage, pg_has_role('has_role_leaf', 'has_role_parent', 'SET') AS leaf_set, pg_has_role('has_role_noinherit', 'has_role_parent', 'MEMBER') AS noinherit_member, pg_has_role('has_role_noinherit', 'has_role_parent', 'USAGE') AS noinherit_usage, pg_has_role('has_role_noinherit', 'has_role_parent', 'SET') AS noinherit_set, pg_has_role('has_role_noinherit', 'has_role_parent', 'USAGE, SET') AS any_privilege",
            &[],
        )
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(row["leaf_member"], Value::Bool(true));
    assert_eq!(row["leaf_usage"], Value::Bool(true));
    assert_eq!(row["leaf_set"], Value::Bool(false));
    assert_eq!(row["noinherit_member"], Value::Bool(true));
    assert_eq!(row["noinherit_usage"], Value::Bool(false));
    assert_eq!(row["noinherit_set"], Value::Bool(true));
    assert_eq!(row["any_privilege"], Value::Bool(true));

    let admin = engine
        .sql(
            "SELECT pg_has_role('has_role_admin', 'has_role_parent', 'MEMBER') AS member, pg_has_role('has_role_admin', 'has_role_parent', 'USAGE') AS usage, pg_has_role('has_role_admin', 'has_role_parent', 'SET') AS can_set, pg_has_role('has_role_admin', 'has_role_parent', 'MEMBER WITH ADMIN OPTION') AS member_admin, pg_has_role('has_role_admin', 'has_role_parent', 'USAGE WITH GRANT OPTION') AS usage_admin, pg_has_role('has_role_admin', 'has_role_parent', 'SET WITH ADMIN OPTION') AS set_admin",
            &[],
        )
        .unwrap();
    assert_eq!(admin.rows.len(), 1);
    assert_eq!(admin.rows[0]["member"], Value::Bool(true));
    assert_eq!(admin.rows[0]["usage"], Value::Bool(false));
    assert_eq!(admin.rows[0]["can_set"], Value::Bool(false));
    for name in ["member_admin", "usage_admin", "set_admin"] {
        assert_eq!(admin.rows[0][name], Value::Bool(true));
    }

    engine.sql("SET ROLE has_role_leaf", &[]).unwrap();
    let current = engine
        .sql(
            "SELECT pg_has_role('has_role_parent', 'MEMBER') AS member, pg_has_role('has_role_parent', 'USAGE') AS usage, pg_has_role('has_role_parent', 'SET') AS can_set",
            &[],
        )
        .unwrap();
    assert_eq!(current.rows[0]["member"], Value::Bool(true));
    assert_eq!(current.rows[0]["usage"], Value::Bool(true));
    assert_eq!(current.rows[0]["can_set"], Value::Bool(false));
    engine.sql("RESET ROLE", &[]).unwrap();
}

#[test]
fn pg18_pg_has_role_name_oid_overloads_errors_strictness_and_catalog_match() {
    let engine = Engine::new();
    create_pg_has_role_graph(&engine);

    let overloads = engine
        .sql(
            "SELECT pg_has_role('has_role_leaf', (SELECT oid FROM pg_roles WHERE rolname = 'has_role_parent'), 'MEMBER') AS name_oid, pg_has_role((SELECT oid FROM pg_roles WHERE rolname = 'has_role_leaf'), 'has_role_parent', 'MEMBER') AS oid_name, pg_has_role((SELECT oid FROM pg_roles WHERE rolname = 'has_role_leaf'), (SELECT oid FROM pg_roles WHERE rolname = 'has_role_parent'), 'MEMBER') AS oid_oid, pg_has_role('has_role_parent', 'MEMBER') AS current_name, pg_has_role((SELECT oid FROM pg_roles WHERE rolname = 'has_role_parent'), 'MEMBER') AS current_oid, pg_has_role('has_role_leaf', 4294967294::oid, 'MEMBER') AS missing_target, pg_has_role(4294967294::oid, 'MEMBER') AS superuser_missing_target",
            &[],
        )
        .unwrap();
    for name in [
        "name_oid",
        "oid_name",
        "oid_oid",
        "current_name",
        "current_oid",
        "superuser_missing_target",
    ] {
        assert_eq!(overloads.rows[0][name], Value::Bool(true));
    }
    assert_eq!(overloads.rows[0]["missing_target"], Value::Bool(false));
    assert_eq!(
        scalar(
            &engine,
            "SELECT pg_has_role('has_role_leaf', 'has_role_parent', NULL) AS v",
        ),
        Value::Null
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT pg_has_role('has_role_leaf', 'missing_role', 'MEMBER')",
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT pg_has_role('has_role_leaf', 'has_role_parent', 'ADMIN')",
        ),
        "22023"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE generated_role_check(value integer, allowed boolean GENERATED ALWAYS AS (pg_has_role('has_role_parent', 'MEMBER')) STORED)",
        ),
        "42P17"
    );

    let catalog = engine
        .sql(
            "SELECT oid, proisstrict, provolatile, proparallel, prorettype, prosrc FROM pg_catalog.pg_proc WHERE proname = 'pg_has_role' ORDER BY oid",
            &[],
        )
        .unwrap();
    let sources = [
        "pg_has_role_name_name",
        "pg_has_role_name_id",
        "pg_has_role_id_name",
        "pg_has_role_id_id",
        "pg_has_role_name",
        "pg_has_role_id",
    ];
    assert_eq!(catalog.rows.len(), sources.len());
    for (index, (row, source)) in catalog.rows.iter().zip(sources).enumerate() {
        assert_eq!(row["oid"], Value::Int(2705 + index as i64));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("s".into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
        assert_eq!(row["prorettype"], Value::Int(16));
        assert_eq!(row["prosrc"], Value::Str(source.into()));
    }
}

fn assert_membership_catalog_and_delegation(engine: &Engine) {
    let memberships = engine
        .sql(
            "SELECT granted.rolname AS granted, member.rolname AS member, grantor.rolname AS grantor, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member JOIN pg_catalog.pg_roles AS grantor ON grantor.oid = membership.grantor WHERE granted.rolname = 'membership_parent' ORDER BY member.rolname, grantor.rolname",
            &[],
        )
        .unwrap();
    assert_eq!(memberships.rows.len(), 5);
    assert_eq!(
        memberships.rows[0]["member"],
        Value::Str("membership_admin".into())
    );
    assert_eq!(memberships.rows[0]["admin_option"], Value::Bool(true));
    assert_eq!(memberships.rows[0]["inherit_option"], Value::Bool(false));
    assert_eq!(memberships.rows[0]["set_option"], Value::Bool(false));
    assert_eq!(
        memberships.rows[2]["member"],
        Value::Str("membership_member".into())
    );
    assert_eq!(memberships.rows[2]["inherit_option"], Value::Bool(true));
    assert_eq!(
        memberships.rows[4]["member"],
        Value::Str("membership_noinherit".into())
    );
    assert_eq!(memberships.rows[4]["inherit_option"], Value::Bool(false));

    assert_eq!(
        sqlstate(engine, "GRANT membership_member TO membership_parent",),
        "0LP01"
    );
    assert_eq!(
        sqlstate(
            engine,
            "REVOKE ADMIN OPTION FOR membership_parent FROM membership_admin RESTRICT",
        ),
        "2BP01"
    );
    engine
        .sql(
            "REVOKE ADMIN OPTION FOR membership_parent FROM membership_admin CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE member.rolname = 'membership_delegate'",
        ),
        Value::Int(0)
    );
    for sql in [
        "GRANT membership_parent TO membership_admin WITH ADMIN TRUE",
        "GRANT membership_parent TO membership_delegate GRANTED BY membership_admin",
        "GRANT membership_parent TO membership_admin WITH ADMIN FALSE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let retained_delegation = engine
        .sql(
            "SELECT administrator.admin_option, count(delegated.oid) AS delegated_count FROM pg_catalog.pg_auth_members AS administrator LEFT JOIN pg_catalog.pg_auth_members AS delegated ON delegated.roleid = administrator.roleid AND delegated.grantor = administrator.member JOIN pg_catalog.pg_roles AS granted ON granted.oid = administrator.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = administrator.member WHERE granted.rolname = 'membership_parent' AND member.rolname = 'membership_admin' GROUP BY administrator.admin_option",
            &[],
        )
        .unwrap();
    assert_eq!(retained_delegation.rows.len(), 1);
    assert_eq!(
        retained_delegation.rows[0]["admin_option"],
        Value::Bool(false)
    );
    assert_eq!(
        retained_delegation.rows[0]["delegated_count"],
        Value::Int(1)
    );
    engine
        .sql(
            "REVOKE membership_parent FROM membership_delegate GRANTED BY membership_admin",
            &[],
        )
        .unwrap();
}

fn assert_membership_execution_and_inheritance(engine: &Engine) {
    for sql in [
        "CREATE FUNCTION membership_probe() RETURNS text LANGUAGE SQL AS 'SELECT current_user'",
        "REVOKE ALL ON FUNCTION membership_probe() FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION membership_probe() TO membership_parent",
        "SET ROLE membership_member",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    assert_eq!(
        scalar(engine, "SELECT membership_probe() AS v"),
        Value::Str("membership_member".into())
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("SET ROLE membership_noinherit", &[]).unwrap();
    assert_eq!(sqlstate(engine, "SELECT membership_probe()"), "42501");
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT membership_parent TO membership_noinherit WITH INHERIT TRUE",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE membership_noinherit", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT membership_probe() AS v"),
        Value::Str("membership_noinherit".into())
    );
    engine.sql("RESET ROLE", &[]).unwrap();

    engine.sql("SET ROLE membership_leaf", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT membership_probe() AS v"),
        Value::Str("membership_leaf".into())
    );
    engine.sql("RESET ROLE", &[]).unwrap();
}

fn assert_membership_assumption_and_createrole(engine: &Engine) {
    for sql in [
        "GRANT membership_leaf TO uqa WITH SET TRUE",
        "GRANT membership_no_set TO uqa WITH SET FALSE",
        "GRANT membership_creator TO uqa",
        "BEGIN",
        "ALTER ROLE uqa NOSUPERUSER NOCREATEROLE",
        "SET ROLE membership_parent",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine.sql("SAVEPOINT before_no_set", &[]).unwrap();
    assert_eq!(sqlstate(engine, "SET ROLE membership_no_set"), "42501");
    engine
        .sql("ROLLBACK TO SAVEPOINT before_no_set", &[])
        .unwrap();
    engine.sql("SET ROLE membership_creator", &[]).unwrap();
    engine.sql("SAVEPOINT before_foreign_alter", &[]).unwrap();
    assert_eq!(
        sqlstate(engine, "ALTER ROLE membership_foreign LOGIN"),
        "42501"
    );
    engine
        .sql("ROLLBACK TO SAVEPOINT before_foreign_alter", &[])
        .unwrap();
    engine.sql("CREATE ROLE membership_owned", &[]).unwrap();
    let creator_grant = engine
        .sql(
            "SELECT membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = 'membership_owned' AND member.rolname = 'membership_creator'",
            &[],
        )
        .unwrap();
    assert_eq!(creator_grant.rows.len(), 1);
    assert_eq!(creator_grant.rows[0]["admin_option"], Value::Bool(true));
    assert_eq!(creator_grant.rows[0]["inherit_option"], Value::Bool(false));
    assert_eq!(creator_grant.rows[0]["set_option"], Value::Bool(false));
    engine
        .sql("ALTER ROLE membership_owned LOGIN", &[])
        .unwrap();
    engine.sql("DROP ROLE membership_owned", &[]).unwrap();
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
}
