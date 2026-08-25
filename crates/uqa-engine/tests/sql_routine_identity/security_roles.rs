//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
