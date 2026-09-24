//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::relation_lock_support::{error, reopen, sessions, sql};
use uqa_core::Value;

#[test]
fn multiple_column_checks_keep_enforcement_inheritance_and_transaction_boundaries() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        drop(peer);
        sql(&engine, "CREATE TABLE declared(v int CHECK(v>0) CHECK(v<10)); CREATE TABLE p(v int); CREATE TABLE c(x int DEFAULT 1) INHERITS(p); CREATE TABLE g() INHERITS(c)");
        for value in [-1, 10] {
            error(
                &engine,
                &format!("INSERT INTO declared VALUES({value})"),
                "23514",
            );
        }
        sql(&engine, "BEGIN; SAVEPOINT original; ALTER TABLE p ADD COLUMN x int DEFAULT 1 CONSTRAINT positive CHECK(x>0) CONSTRAINT upper CHECK(x<10) CONSTRAINT local_only CHECK(x<>5) NO INHERIT CONSTRAINT inactive CHECK(x<0) NOT ENFORCED");
        for table in ["p", "c", "g"] {
            for value in [-1, 10] {
                error(
                    &engine,
                    &format!("INSERT INTO {table}(x) VALUES({value})"),
                    "23514",
                );
                sql(&engine, "ROLLBACK TO original");
                sql(&engine, "ALTER TABLE p ADD COLUMN x int DEFAULT 1 CONSTRAINT positive CHECK(x>0) CONSTRAINT upper CHECK(x<10) CONSTRAINT local_only CHECK(x<>5) NO INHERIT CONSTRAINT inactive CHECK(x<0) NOT ENFORCED");
            }
        }
        sql(&engine, "ROLLBACK TO original; COMMIT");
        assert_eq!(sql(&engine, "SELECT * FROM p").columns, ["v"]);
        sql(&engine, "ALTER TABLE p ADD COLUMN x int DEFAULT 1 CONSTRAINT positive CHECK(x>0) CONSTRAINT upper CHECK(x<10) CONSTRAINT local_only CHECK(x<>5) NO INHERIT CONSTRAINT inactive CHECK(x<0) NOT ENFORCED; ALTER TABLE p ADD COLUMN IF NOT EXISTS x int CHECK(x<0) CHECK(x>100)");
        error(&engine, "INSERT INTO p(x) VALUES(5)", "23514");
        sql(
            &engine,
            "INSERT INTO c(x) VALUES(5); INSERT INTO g(x) VALUES(5); INSERT INTO p(x) VALUES(2)",
        );
        let state = sql(&engine, "SELECT conname, conenforced, connoinherit FROM pg_constraint WHERE conrelid='p'::regclass ORDER BY conname").rows;
        assert_eq!(state.len(), 4);
        assert_eq!(state[0]["conenforced"], Value::Bool(false));
        assert_eq!(state[1]["connoinherit"], Value::Bool(true));
        drop(engine);
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(sql(&engine, "SELECT conname, conenforced, connoinherit FROM pg_constraint WHERE conrelid='p'::regclass ORDER BY conname").rows, state);
        for table in ["p", "c", "g"] {
            error(
                &engine,
                &format!("INSERT INTO {table}(x) VALUES(10)"),
                "23514",
            );
            sql(&engine, &format!("INSERT INTO {table}(x) VALUES(2)"));
        }
    }
}
