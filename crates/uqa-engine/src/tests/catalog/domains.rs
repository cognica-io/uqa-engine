//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain catalog publication must preserve independent concurrent declarations.

use crate::tests::relation_lock_support::{before_commit, reopen, sessions, sql};
use uqa_core::Value;

#[test]
fn domains_with_matching_constraint_names_commit_independently() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(
            &first,
            "BEGIN; CREATE DOMAIN a AS int CONSTRAINT shared CHECK(VALUE>0)",
        );
        let second = before_commit(
            &first,
            second,
            "CREATE DOMAIN b AS int CONSTRAINT shared CHECK(VALUE<100)",
        );
        sql(&first, "SELECT 1::a, 1::b");
        drop(second);
        drop(first);
        let restored = reopen(provider, &directory.path().join("table-locks.db"));
        sql(&restored, "SELECT 1::a, 1::b");
    }
}

#[test]
fn independent_domain_creations_and_deletions_commit_without_lost_records() {
    for provider in 0..3 {
        for (holder, worker, expected) in [
            ("DROP DOMAIN a", "CREATE DOMAIN c AS int", vec!["b", "c"]),
            ("CREATE DOMAIN c AS int", "DROP DOMAIN a", vec!["b", "c"]),
            ("DROP DOMAIN a", "DROP DOMAIN b", vec![]),
        ] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE DOMAIN a AS int; CREATE DOMAIN b AS int");
            sql(&first, &format!("BEGIN; {holder}"));
            let second = before_commit(&first, second, worker);
            let names = |engine: &crate::Engine| {
                sql(
                    engine,
                    "SELECT typname FROM pg_type WHERE typtype='d' AND typnamespace=(SELECT oid FROM pg_namespace WHERE nspname='public') ORDER BY typname",
                )
                .rows
                .into_iter()
                .map(|row| row["typname"].clone())
                .collect::<Vec<_>>()
            };
            let expected = expected
                .into_iter()
                .map(|name| Value::Str(name.into()))
                .collect::<Vec<_>>();
            assert_eq!(names(&first), expected);
            assert_eq!(names(&second), expected);
            drop((first, second));
            let restored = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(names(&restored), expected);
        }
    }
}

#[test]
fn private_domain_records_survive_peer_publication_and_undo_at_each_isolation() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO changes; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE DOMAIN removed AS int");
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT changes; DROP DOMAIN removed; CREATE DOMAIN private AS int CONSTRAINT shared CHECK(VALUE>0)"));
                let private = first.durable.domains.read()["public.private"].object_id;
                sql(&second, "CREATE DOMAIN peer AS int CONSTRAINT shared CHECK(VALUE<100); INSERT INTO t VALUES(2)");
                first.list_named_analyzers().unwrap();
                if isolation == "READ COMMITTED" {
                    sql(&first, "SELECT typname FROM pg_type");
                }
                let registry = first.durable.domains.snapshot();
                assert_eq!(registry["public.private"].object_id, private);
                assert!(
                    registry.contains_key("public.peer"),
                    "{provider}/{isolation}/{finish}"
                );
                assert!(!registry.contains_key("public.removed"));
                assert_eq!(
                    sql(&first, "SELECT count(*) AS n FROM t").rows[0]["n"],
                    Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
                );
                sql(&first, finish);
                sql(&first, "SELECT 1::peer");
                drop((first, second));
                let restored = reopen(provider, &directory.path().join("table-locks.db"));
                let registry = restored.durable.domains.read();
                assert!(registry.contains_key("public.peer"));
                assert_eq!(registry.contains_key("public.private"), finish == "COMMIT");
                assert_eq!(registry.contains_key("public.removed"), finish != "COMMIT");
                if finish == "COMMIT" {
                    assert_eq!(registry["public.private"].object_id, private);
                }
            }
        }
    }
}
