//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain catalog publication must preserve independent concurrent declarations.

use crate::tests::relation_lock_support::{before_commit, error, reopen, sessions, sql};
use uqa_core::Value;

mod constraint_identities;
mod type_names;

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

#[test]
fn domain_cascade_preserves_constraint_ownership_and_undo_across_reopen() {
    for provider in 0..3 {
        for removal in [
            "DROP DOMAIN removed.d CASCADE",
            "DROP SCHEMA removed CASCADE",
        ] {
            let (directory, engine, peer) = sessions(provider);
            drop(peer);
            sql(&engine, "CREATE SCHEMA removed; CREATE DOMAIN removed.d AS int; CREATE TABLE domain_keys(id removed.d PRIMARY KEY, u removed.d UNIQUE, keep int, twice int GENERATED ALWAYS AS (id::int * 2) STORED); CREATE TABLE domain_ref(id int REFERENCES domain_keys(id)); CREATE VIEW domain_view AS SELECT twice FROM domain_keys; CREATE INDEX domain_plain ON domain_keys(id); CREATE INDEX domain_expression ON domain_keys(((keep::removed.d)::int)); CREATE INDEX domain_predicate ON domain_keys(keep) WHERE (keep::removed.d)::int > 0; CREATE INDEX domain_keep ON domain_keys(keep); INSERT INTO domain_keys(id,u,keep) VALUES (2,4,3); INSERT INTO domain_ref VALUES (2)");
            let indexes = |engine: &crate::Engine| {
                sql(engine, "SELECT indexname FROM pg_indexes WHERE tablename='domain_keys' ORDER BY indexname").rows
            };
            let original = indexes(&engine);
            assert_eq!(original.len(), 6);
            error(&engine, "DROP INDEX domain_keys_pkey CASCADE", "2BP01");
            error(&engine, "DROP DOMAIN removed.d", "2BP01");
            sql(&engine, &format!("BEGIN; SAVEPOINT kept; {removal}"));
            assert_eq!(indexes(&engine).len(), 1);
            assert_eq!(sql(&engine, "SELECT * FROM domain_keys").columns, ["keep"]);
            sql(&engine, "ROLLBACK TO kept; COMMIT");
            drop(engine);
            let path = directory.path().join("table-locks.db");
            let engine = reopen(provider, &path);
            assert_eq!(indexes(&engine), original);
            assert_eq!(
                sql(&engine, "SELECT twice FROM domain_view").rows[0]["twice"],
                Value::Int(4)
            );
            error(&engine, "INSERT INTO domain_ref VALUES (9)", "23503");
            sql(&engine, removal);
            drop(engine);
            let engine = reopen(provider, &path);
            let rows = indexes(&engine);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["indexname"], Value::Str("domain_keep".into()));
            let rows = sql(&engine, "SELECT * FROM domain_keys");
            assert_eq!(rows.columns, ["keep"]);
            assert_eq!(rows.rows[0]["keep"], Value::Int(3));
            sql(&engine, "INSERT INTO domain_ref VALUES (9)");
            error(&engine, "SELECT * FROM domain_view", "42P01");
        }
    }
}
