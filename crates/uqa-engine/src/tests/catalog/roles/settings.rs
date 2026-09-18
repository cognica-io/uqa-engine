//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit role names retain their case and stay distinct from parameter reset.

use crate::tests::relation_lock_support::{error, sessions, sql};

#[test]
fn explicit_role_names_and_local_resets_keep_their_postgresql_meaning() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        for name in ["NONE", "DEFAULT", "default"] {
            sql(&engine, &format!("CREATE ROLE \"{name}\""));
            for statement in [
                format!("SET ROLE \"{name}\""),
                format!("SET ROLE '{name}'"),
                format!("SET role TO '{name}'"),
            ] {
                sql(&engine, &statement);
                assert_eq!(
                    engine
                        .current_role()
                        .require_name(&engine.durable.roles.read())
                        .unwrap(),
                    name
                );
                for missing in ["SET ROLE missing_role", "SET ROLE ''"] {
                    error(&engine, missing, "22023");
                    assert_eq!(
                        engine
                            .current_role()
                            .require_name(&engine.durable.roles.read())
                            .unwrap(),
                        name
                    );
                }
                for reset in ["RESET ROLE", "SET ROLE NONE", "SET ROLE TO DEFAULT"] {
                    sql(&engine, &statement);
                    sql(&engine, reset);
                    assert_eq!(
                        engine
                            .current_role()
                            .require_name(&engine.durable.roles.read())
                            .unwrap(),
                        "uqa"
                    );
                }
            }
        }
        error(&engine, "SET ROLE DEFAULT", "42601");
        sql(&engine, "BEGIN; SET LOCAL ROLE \"DEFAULT\"");
        assert_eq!(
            engine
                .current_role()
                .require_name(&engine.durable.roles.read())
                .unwrap(),
            "DEFAULT"
        );
        sql(&engine, "COMMIT");
        assert_eq!(
            engine
                .current_role()
                .require_name(&engine.durable.roles.read())
                .unwrap(),
            "uqa"
        );
        sql(&engine, "BEGIN; SET ROLE \"NONE\"; SAVEPOINT selected; SET ROLE TO DEFAULT; ROLLBACK TO selected");
        assert_eq!(
            engine
                .current_role()
                .require_name(&engine.durable.roles.read())
                .unwrap(),
            "NONE"
        );
        sql(&engine, "ROLLBACK");
        assert_eq!(
            engine
                .current_role()
                .require_name(&engine.durable.roles.read())
                .unwrap(),
            "uqa"
        );
    }
}
