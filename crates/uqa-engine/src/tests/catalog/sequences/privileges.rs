//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::relation_lock_support::{sessions, sql};
use uqa_core::{RelationIdentity, Value};

mod relations;

#[test]
fn sequence_privilege_inquiry_keeps_committed_roles_and_acl_together() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(&engine, "CREATE ROLE reader; CREATE ROLE readers; GRANT readers TO reader; CREATE SEQUENCE ids; GRANT USAGE ON SEQUENCE ids TO readers; GRANT SELECT ON t TO reader; SET ROLE reader");
            let oid = uqa_execution::catalog::projection::sequence_relation_oid(
                engine.durable.sequence_object_ids.read()[&RelationIdentity::new("public", "ids")],
            );
            let reader_oid = engine.durable.roles.read()["reader"].oid;
            sql(
                &engine,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&peer, "BEGIN; REVOKE readers FROM reader; CREATE ROLE new_owner; GRANT CREATE ON SCHEMA public TO new_owner; ALTER SEQUENCE ids OWNER TO new_owner; INSERT INTO t VALUES (2); COMMIT");
            for target in [Value::Int(oid), Value::Str("ids".into())] {
                for subject in [
                    None,
                    Some(Value::Str("reader".into())),
                    Some(Value::Int(reader_oid)),
                ] {
                    let mut arguments = subject.into_iter().collect::<Vec<_>>();
                    arguments.extend([target.clone(), Value::Str("USAGE".into())]);
                    let result = engine
                        .scalar_function_context()
                        .sequence_privileges
                        .has_sequence_privilege_value(&arguments)
                        .unwrap();
                    assert_eq!(
                        result,
                        Value::Bool(false),
                        "{provider}: {isolation}: {arguments:?}"
                    );
                    let roles = engine.durable.roles.read();
                    for security in engine.durable.sequence_security.read().values() {
                        let security = security.resolve(&roles).unwrap();
                        assert!(
                            roles.contains_key(&security.role_owner),
                            "inquiry split live roles and sequence owners"
                        );
                    }
                }
            }
            assert_eq!(
                sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
            );
            sql(&engine, "ROLLBACK");
        }
    }
}

#[test]
fn sequence_privilege_inquiry_retains_private_and_temporary_acl_changes() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(
                &engine,
                "CREATE ROLE reader; CREATE SEQUENCE ids; CREATE TEMP SEQUENCE temporary_ids",
            );
            sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private_acl; CREATE ROLE private_reader; CREATE SEQUENCE private_ids; GRANT USAGE ON SEQUENCE ids, private_ids, temporary_ids TO private_reader"));
            sql(&peer, "CREATE ROLE peer_role; INSERT INTO t VALUES (2)");
            for name in ["ids", "private_ids", "temporary_ids"] {
                assert_eq!(sql(&engine, &format!("SELECT has_sequence_privilege('private_reader', '{name}', 'USAGE') AS v")).rows[0]["v"], Value::Bool(true), "{provider}: {isolation}: {name}");
            }
            sql(&engine, "ROLLBACK TO private_acl");
            for name in ["ids", "temporary_ids"] {
                assert_eq!(
                    sql(
                        &engine,
                        &format!("SELECT has_sequence_privilege('reader', '{name}', 'USAGE') AS v")
                    )
                    .rows[0]["v"],
                    Value::Bool(false)
                );
            }
            assert_eq!(
                sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
            );
            sql(&engine, "ROLLBACK");
        }
    }
}
