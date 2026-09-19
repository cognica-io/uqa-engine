//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Quoted ACL recipients keep their own privileges, dependencies and durable grant paths.

use super::identity::reopen;
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use uqa_core::Value;

struct Target {
    grant: &'static str,
    inquiry: &'static str,
}

const TARGETS: &[Target] = &[
    Target {
        grant: "SELECT ON TABLE acl_table",
        inquiry: "has_table_privilege('{role}', 'acl_table', 'SELECT')",
    },
    Target {
        grant: "SELECT(v) ON TABLE acl_columns",
        inquiry: "has_column_privilege('{role}', 'acl_columns', 'v', 'SELECT')",
    },
    Target {
        grant: "SELECT ON TABLE acl_view",
        inquiry: "has_table_privilege('{role}', 'acl_view', 'SELECT')",
    },
    Target {
        grant: "SELECT ON TABLE acl_matview",
        inquiry: "has_table_privilege('{role}', 'acl_matview', 'SELECT')",
    },
    Target {
        grant: "SELECT ON TABLE acl_foreign",
        inquiry: "has_table_privilege('{role}', 'acl_foreign', 'SELECT')",
    },
    Target {
        grant: "USAGE ON SEQUENCE acl_sequence",
        inquiry: "has_sequence_privilege('{role}', 'acl_sequence', 'USAGE')",
    },
    Target {
        grant: "CREATE ON DATABASE uqa",
        inquiry: "has_database_privilege('{role}', 'uqa', 'CREATE')",
    },
    Target {
        grant: "USAGE ON SCHEMA acl_schema",
        inquiry: "has_schema_privilege('{role}', 'acl_schema', 'USAGE')",
    },
    Target {
        grant: "EXECUTE ON FUNCTION acl_function()",
        inquiry: "has_function_privilege('{role}', 'acl_function()', 'EXECUTE')",
    },
    Target {
        grant: "SELECT ON pg_catalog.pg_authid",
        inquiry: "has_table_privilege('{role}', 'pg_catalog.pg_authid', 'SELECT')",
    },
    Target {
        grant: "UPDATE(rolname) ON pg_catalog.pg_authid",
        inquiry: "has_column_privilege('{role}', 'pg_catalog.pg_authid', 'rolname', 'UPDATE')",
    },
];

fn assert_privilege(engine: &Engine, target: &Target, role: &str, expected: bool) {
    let expression = target.inquiry.replace("{role}", role);
    assert_eq!(
        sql(engine, &format!("SELECT {expression} AS permitted")).rows[0]["permitted"],
        Value::Bool(expected),
        "{} for {role}",
        target.grant
    );
}

fn setup(engine: &Engine) {
    sql(
        engine,
        r#"CREATE ROLE "PUBLIC"; CREATE ROLE "CURRENT_USER"; CREATE ROLE "SESSION_USER"; CREATE ROLE unrelated; CREATE TABLE acl_table(v int); CREATE TABLE acl_columns(v int); CREATE VIEW acl_view AS SELECT v FROM acl_table; CREATE MATERIALIZED VIEW acl_matview AS SELECT v FROM acl_table; CREATE SERVER acl_remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE acl_foreign(v int) SERVER acl_remote; CREATE SEQUENCE acl_sequence; CREATE SCHEMA acl_schema; CREATE FUNCTION acl_function() RETURNS int LANGUAGE SQL AS 'SELECT 1'; REVOKE EXECUTE ON FUNCTION acl_function() FROM PUBLIC"#,
    );
}

#[test]
fn quoted_grantees_and_public_keep_independent_rights_after_refresh_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        setup(&first);
        for target in TARGETS {
            sql(
                &first,
                &format!(
                    r#"GRANT {} TO "PUBLIC", "CURRENT_USER", "SESSION_USER" WITH GRANT OPTION"#,
                    target.grant
                ),
            );
            for role in ["PUBLIC", "CURRENT_USER", "SESSION_USER"] {
                assert_privilege(&second, target, role, true);
            }
            assert_privilege(&second, target, "unrelated", false);
            sql(&first, &format!("GRANT {} TO PUBLIC", target.grant));
            sql(&first, &format!(r#"REVOKE {} FROM "PUBLIC""#, target.grant));
            assert_privilege(&second, target, "PUBLIC", true);
            assert_privilege(&second, target, "unrelated", true);
            sql(
                &first,
                &format!(r#"GRANT {} TO "PUBLIC" WITH GRANT OPTION"#, target.grant),
            );
            sql(&first, &format!("REVOKE {} FROM PUBLIC", target.grant));
            assert_privilege(&second, target, "PUBLIC", true);
            assert_privilege(&second, target, "unrelated", false);
        }
        for role in ["PUBLIC", "CURRENT_USER", "SESSION_USER"] {
            error(&first, &format!(r#"DROP ROLE "{role}""#), "2BP01");
        }
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        for target in TARGETS {
            for role in ["PUBLIC", "CURRENT_USER", "SESSION_USER"] {
                assert_privilege(&reopened, target, role, true);
            }
            assert_privilege(&reopened, target, "unrelated", false);
        }
    }
}

#[test]
fn named_public_is_a_real_grantor_and_cascade_preserves_other_grant_paths() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        setup(&first);
        for target in TARGETS {
            sql(
                &first,
                &format!(
                    r#"GRANT {} TO "PUBLIC", "CURRENT_USER" WITH GRANT OPTION"#,
                    target.grant
                ),
            );
            sql(&first, r#"SET ROLE "PUBLIC""#);
            sql(
                &first,
                &format!(r#"GRANT {} TO unrelated GRANTED BY "PUBLIC""#, target.grant),
            );
            sql(&first, "RESET ROLE");
            sql(&first, r#"SET ROLE "CURRENT_USER""#);
            sql(
                &first,
                &format!(
                    r#"GRANT {} TO unrelated GRANTED BY "CURRENT_USER""#,
                    target.grant
                ),
            );
            sql(&first, "RESET ROLE");
            sql(
                &first,
                &format!(r#"REVOKE {} FROM "PUBLIC" CASCADE"#, target.grant),
            );
            assert_privilege(&second, target, "PUBLIC", false);
            assert_privilege(&second, target, "unrelated", true);
        }
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        for target in TARGETS {
            assert_privilege(&reopened, target, "unrelated", true);
            sql(
                &reopened,
                &format!(r#"REVOKE {} FROM "CURRENT_USER" CASCADE"#, target.grant),
            );
            assert_privilege(&reopened, target, "unrelated", false);
        }
    }
}

#[test]
fn quoted_missing_grantees_and_grantors_are_looked_up_as_literal_role_names() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE items(v int)");
    for role in ["PUBLIC", "CURRENT_USER", "SESSION_USER"] {
        error(
            &engine,
            &format!(r#"GRANT SELECT ON items TO "{role}""#),
            "42704",
        );
        error(
            &engine,
            &format!(r#"GRANT SELECT ON items TO PUBLIC GRANTED BY "{role}""#),
            "42704",
        );
    }
    sql(
        &engine,
        "GRANT SELECT ON items TO CURRENT_USER GRANTED BY SESSION_USER",
    );
}

#[test]
fn temporary_acl_dependencies_distinguish_named_public_from_public_access() {
    for provider in 0..3 {
        for (create, privilege) in [
            (
                "CREATE TEMP TABLE local_acl(v int)",
                "SELECT ON TABLE local_acl",
            ),
            (
                "CREATE TEMP TABLE local_acl(v int)",
                "SELECT(v) ON TABLE local_acl",
            ),
            (
                "CREATE TEMP VIEW local_acl AS SELECT 1 AS v",
                "SELECT ON TABLE local_acl",
            ),
            (
                "CREATE TEMP SEQUENCE local_acl",
                "USAGE ON SEQUENCE local_acl",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, r#"CREATE ROLE "PUBLIC""#);
            sql(&first, create);
            sql(&first, &format!("GRANT {privilege} TO PUBLIC"));
            sql(&second, r#"DROP ROLE "PUBLIC"; CREATE ROLE "PUBLIC""#);
            sql(&first, &format!(r#"GRANT {privilege} TO "PUBLIC""#));
            error(&second, r#"DROP ROLE "PUBLIC""#, "2BP01");
            sql(&first, &format!(r#"REVOKE {privilege} FROM "PUBLIC""#));
            sql(&second, r#"DROP ROLE "PUBLIC""#);
        }
    }
}

#[test]
fn column_privilege_catalogs_preserve_public_names_and_distinct_grantability() {
    let engine = Engine::new();
    sql(
        &engine,
        r#"CREATE ROLE "PUBLIC"; CREATE ROLE observer; CREATE TABLE named_acl(v int); GRANT SELECT(v) ON named_acl TO "PUBLIC"; SET ROLE observer"#,
    );
    let count = |view: &str, extra: &str| {
        sql(&engine, &format!("SELECT count(*) AS n FROM information_schema.{view} WHERE table_name = 'named_acl' AND grantee = 'PUBLIC' {extra}")).rows[0]["n"].clone()
    };
    assert_eq!(count("column_privileges", ""), Value::Int(1));
    assert_eq!(count("role_column_grants", ""), Value::Int(0));
    sql(
        &engine,
        r#"RESET ROLE; GRANT SELECT(v) ON named_acl TO PUBLIC; SET ROLE "PUBLIC""#,
    );
    assert_eq!(count("column_privileges", ""), Value::Int(2));
    assert_eq!(count("role_column_grants", ""), Value::Int(2));
    sql(
        &engine,
        r#"RESET ROLE; ALTER TABLE named_acl OWNER TO "PUBLIC""#,
    );
    for grantable in ["NO", "YES"] {
        assert_eq!(
            count(
                "column_privileges",
                &format!("AND privilege_type = 'SELECT' AND is_grantable = '{grantable}'")
            ),
            Value::Int(1)
        );
    }
}
