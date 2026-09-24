//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role deletion validates each literal target before publishing any removal.

use super::identity::reopen;
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use uqa_core::Value;

mod order;

#[test]
fn drop_special_role_specifiers_preserve_permission_precedence_and_literal_names() {
    let engine = Engine::new();
    sql(
        &engine,
        r#"CREATE ROLE plain; CREATE ROLE creator CREATEROLE; CREATE ROLE "CURRENT_USER"; CREATE ROLE "CURRENT_ROLE"; CREATE ROLE "SESSION_USER"; CREATE ROLE "PUBLIC""#,
    );
    let original = engine.durable.roles.read().clone();
    for actor in ["uqa", "creator", "plain"] {
        sql(&engine, &format!("RESET ROLE; SET ROLE {actor}"));
        for target in [
            "CURRENT_USER",
            "CURRENT_ROLE",
            "SESSION_USER",
            "PUBLIC",
            r#""public""#,
        ] {
            for missing in ["", "IF EXISTS "] {
                error(
                    &engine,
                    &format!("DROP ROLE {missing}{target}"),
                    if actor == "plain" { "42501" } else { "22023" },
                );
            }
        }
        assert_eq!(*engine.durable.roles.read(), original);
    }
    sql(&engine, "RESET ROLE");
    for target in ["CURRENT_USER", "CURRENT_ROLE", "SESSION_USER", "PUBLIC"] {
        sql(&engine, &format!(r#"DROP ROLE "{target}""#));
        assert!(!engine.durable.roles.read().contains_key(target));
    }
    assert_eq!(
        sql(&engine, "SELECT current_user AS who").rows[0]["who"],
        Value::Str("uqa".into())
    );
}

#[test]
fn drop_preflight_rejects_special_targets_without_partial_role_publication() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE retained");
        let original = first.durable.roles.read().clone();
        sql(&first, "BEGIN; SAVEPOINT keep");
        error(&first, "DROP ROLE retained, CURRENT_USER", "22023");
        sql(&first, "ROLLBACK TO keep");
        error(&first, "DROP ROLE IF EXISTS absent, CURRENT_USER", "22023");
        assert_eq!(
            first.take_sql_notices(),
            vec![(
                "NOTICE".into(),
                "role \"absent\" does not exist, skipping".into()
            )]
        );
        sql(&first, "ROLLBACK TO keep");
        error(&first, "DROP ROLE absent, CURRENT_USER", "42704");
        assert!(first.take_sql_notices().is_empty());
        sql(&first, "ROLLBACK TO keep; COMMIT");
        assert_eq!(*first.durable.roles.read(), original);
        sql(&second, "SELECT rolname FROM pg_roles");
        assert_eq!(*second.durable.roles.read(), original);
        drop(second);
        drop(first);
        assert_eq!(
            *reopen(provider, &directory.path().join("table-locks.db"))
                .durable
                .roles
                .read(),
            original
        );
    }
}
