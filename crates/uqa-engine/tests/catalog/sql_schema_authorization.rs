//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;

fn owner(engine: &Engine, schema: &str) -> String {
    let result=engine.sql("SELECT r.rolname AS owner FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles r ON r.oid = n.nspowner WHERE n.nspname = $1",&[uqa_engine::SQLParam::Scalar(Value::Str(schema.into()))]).unwrap();
    let Some(Value::Str(owner)) = result.rows[0].get("owner") else {
        panic!("expected owner")
    };
    owner.clone()
}
fn assert_error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

#[test]
fn current_user_schema_authorization_is_created_and_reopened_with_its_owner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema_owner.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql("CREATE SCHEMA owned AUTHORIZATION CURRENT_USER", &[])
            .unwrap();
        assert_eq!(owner(&engine, "owned"), "uqa");
    }
    let engine = Engine::open(&path).unwrap();
    assert!(engine.has_schema("owned").unwrap());
    assert_eq!(owner(&engine, "owned"), "uqa");
}

#[test]
fn omitted_schema_names_and_authorization_keywords_use_the_executing_session() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE actor; GRANT CREATE ON DATABASE uqa TO actor; SET ROLE actor; CREATE SCHEMA AUTHORIZATION CURRENT_ROLE; CREATE SCHEMA named AUTHORIZATION CURRENT_USER",&[]).unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA session_owned AUTHORIZATION SESSION_USER",
        "42501",
        "must be able to SET ROLE",
    );
    // A SET ROLE session cannot assign its schema to the session user without that SET ROLE authority.
    assert_eq!(owner(&engine, "actor"), "actor");
    assert_eq!(owner(&engine, "named"), "actor");
    assert!(!engine.has_schema("session_owned").unwrap());
    engine
        .sql("RESET ROLE; CREATE SCHEMA AUTHORIZATION SESSION_USER", &[])
        .unwrap();
    assert_eq!(owner(&engine, "uqa"), "uqa");
}

#[test]
fn quoted_current_user_role_is_not_treated_as_an_authorization_keyword() {
    let engine = Engine::new();
    engine.sql(r#"CREATE ROLE "CURRENT_USER"; CREATE SCHEMA AUTHORIZATION "CURRENT_USER"; CREATE SCHEMA literal_owned AUTHORIZATION "CURRENT_USER""#,&[]).unwrap();
    assert_eq!(owner(&engine, "CURRENT_USER"), "CURRENT_USER");
    assert_eq!(owner(&engine, "literal_owned"), "CURRENT_USER");
}

#[test]
fn schema_authorization_errors_follow_role_database_set_role_and_reserved_name_order() {
    let engine = Engine::new();
    engine
        .sql("CREATE ROLE actor; CREATE ROLE target; SET ROLE actor", &[])
        .unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA pg_blocked AUTHORIZATION absent",
        "42704",
        "does not exist",
    );
    assert_error(
        &engine,
        "CREATE SCHEMA pg_blocked AUTHORIZATION target",
        "42501",
        "permission denied for database",
    );
    engine
        .sql(
            "RESET ROLE; GRANT CREATE ON DATABASE uqa TO actor; SET ROLE actor",
            &[],
        )
        .unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA pg_blocked AUTHORIZATION target",
        "42501",
        "must be able to SET ROLE",
    );
    engine
        .sql(
            "RESET ROLE; GRANT target TO actor WITH INHERIT FALSE, SET TRUE; SET ROLE actor",
            &[],
        )
        .unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA pg_blocked AUTHORIZATION target",
        "42939",
        "unacceptable schema name",
    );
    assert!(!engine.has_schema("pg_blocked").unwrap());
}

#[test]
fn schema_authorization_requires_database_create_from_the_invoker_and_not_the_owner() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE actor; CREATE ROLE target; GRANT CREATE ON DATABASE uqa TO actor; GRANT target TO actor WITH INHERIT FALSE, SET TRUE; SET ROLE actor; CREATE SCHEMA delegated AUTHORIZATION target",&[]).unwrap();
    assert_eq!(owner(&engine, "delegated"), "target");
    let result=engine.sql("SELECT current_user AS actor, has_database_privilege('target','uqa','CREATE') AS target_create",&[]).unwrap();
    assert_eq!(
        result.rows[0].get("actor"),
        Some(&Value::Str("actor".into()))
    );
    assert_eq!(
        result.rows[0].get("target_create"),
        Some(&Value::Bool(false))
    );
}

#[test]
fn schema_if_not_exists_checks_authorization_and_keeps_the_existing_owner() {
    let engine = Engine::new();
    engine
        .sql("CREATE ROLE target; CREATE SCHEMA existing", &[])
        .unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA IF NOT EXISTS existing AUTHORIZATION absent",
        "42704",
        "does not exist",
    );
    engine
        .sql(
            "CREATE SCHEMA IF NOT EXISTS existing AUTHORIZATION target",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [(
            "NOTICE".into(),
            "schema \"existing\" already exists, skipping".into()
        )]
    );
    assert_eq!(owner(&engine, "existing"), "uqa");
    assert_error(
        &engine,
        "CREATE SCHEMA existing AUTHORIZATION target",
        "42P06",
        "already exists",
    );
}

#[test]
fn schema_authorization_follows_transaction_and_savepoint_rollback_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema_rollback.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE ROLE target", &[]).unwrap();
        engine.sql("BEGIN; CREATE SCHEMA rolled_back AUTHORIZATION target; ROLLBACK; BEGIN; CREATE SCHEMA kept AUTHORIZATION target; SAVEPOINT nested; CREATE SCHEMA discarded AUTHORIZATION target; ROLLBACK TO nested; COMMIT",&[]).unwrap();
        assert!(!engine.has_schema("rolled_back").unwrap());
        assert!(!engine.has_schema("discarded").unwrap());
        assert_eq!(owner(&engine, "kept"), "target");
    }
    let engine = Engine::open(&path).unwrap();
    assert!(!engine.has_schema("rolled_back").unwrap());
    assert!(!engine.has_schema("discarded").unwrap());
    assert_eq!(owner(&engine, "kept"), "target");
}

#[test]
fn schema_owner_transfer_checks_database_create_on_the_invoking_owner() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE actor; CREATE ROLE target; GRANT CREATE ON DATABASE uqa TO actor; GRANT target TO actor WITH INHERIT FALSE, SET TRUE; CREATE SCHEMA delegated AUTHORIZATION actor; SET ROLE actor; ALTER SCHEMA delegated OWNER TO target",&[]).unwrap();
    assert_eq!(owner(&engine, "delegated"), "target");
    engine.sql("RESET ROLE; REVOKE target FROM actor; GRANT actor TO target WITH INHERIT FALSE, SET TRUE; SET ROLE target",&[]).unwrap();
    assert_error(
        &engine,
        "ALTER SCHEMA delegated OWNER TO actor",
        "42501",
        "permission denied for database",
    );
    assert_eq!(owner(&engine, "delegated"), "target");
}

#[test]
fn existing_system_schemas_use_duplicate_checks_after_role_authorization() {
    let engine = Engine::new();
    for name in ["ag_catalog", "information_schema"] {
        let before = owner(&engine, name);
        let error = engine
            .sql(&format!("CREATE SCHEMA {name}"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P06"));
        assert_eq!(
            error.to_string(),
            format!(r#"schema "{name}" already exists"#)
        );
        engine
            .sql(&format!("CREATE SCHEMA IF NOT EXISTS {name}"), &[])
            .unwrap();
        assert_eq!(owner(&engine, name), before);
        assert_error(
            &engine,
            &format!("CREATE SCHEMA IF NOT EXISTS {name} AUTHORIZATION missing_owner"),
            "42704",
            "does not exist",
        );
    }
    for sql in [
        "CREATE SCHEMA pg_catalog",
        "CREATE SCHEMA IF NOT EXISTS pg_catalog",
    ] {
        assert_error(&engine, sql, "42939", "unacceptable schema name");
    }
    engine
        .sql("CREATE ROLE without_create; SET ROLE without_create", &[])
        .unwrap();
    assert_error(
        &engine,
        "CREATE SCHEMA IF NOT EXISTS information_schema",
        "42501",
        "permission denied for database",
    );
}
