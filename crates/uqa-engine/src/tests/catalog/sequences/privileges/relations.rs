//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::cell::RefCell;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    catalog::{
        resolution::RelationResolution, security::sequence_inquiry::SequencePrivilegeResolution,
    },
    SQLError,
};

struct AfterResolution<'a> {
    engine: &'a Engine,
    publish: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

impl SequencePrivilegeResolution for AfterResolution<'_> {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        let result = SequencePrivilegeResolution::visible_relation_kind(self.engine, reference)?;
        if let Some(publish) = self.publish.borrow_mut().take() {
            publish();
        }
        Ok(result)
    }
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        SequencePrivilegeResolution::sequence_privilege_oid(self.engine, oid)
    }
}

#[test]
fn relation_inquiries_keep_sequence_acl_and_membership_from_one_committed_view() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for column in [false, true] {
                let (_directory, engine, peer) = sessions(provider);
                sql(&engine, "CREATE ROLE reader; CREATE SEQUENCE ids; GRANT SELECT ON t TO reader; SET ROLE reader");
                sql(
                    &engine,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                let resolution = AfterResolution {
                    engine: &engine,
                    publish: RefCell::new(Some(Box::new(|| {
                        sql(&peer, "BEGIN; CREATE ROLE late_grantee; GRANT SELECT ON SEQUENCE ids TO late_grantee; GRANT late_grantee TO reader; INSERT INTO t VALUES (2); COMMIT");
                    }))),
                };
                let mut context = engine.table_privilege_context();
                context.sequences.resolution = &resolution;
                let mut arguments = vec![Value::Str("ids".into())];
                if column {
                    arguments.push(Value::Str("last_value".into()));
                }
                arguments.push(Value::Str("UPDATE, SELECT".into()));
                let result = if column {
                    context.has_column_privilege_value(&arguments)
                } else {
                    context.has_table_privilege_value(&arguments)
                }
                .unwrap();
                assert_eq!(
                    result,
                    Value::Bool(true),
                    "{provider}: {isolation}: {arguments:?}"
                );
                assert_eq!(
                    sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                    Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
                );
                sql(&engine, "ROLLBACK");
            }
        }
    }
}

#[test]
fn relation_inquiries_bind_new_committed_roles_before_resolving_sequence_targets() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(&engine, "CREATE SEQUENCE ids");
            let oid = uqa_execution::catalog::projection::sequence_relation_oid(
                engine.durable.sequence_object_ids.read()[&RelationIdentity::new("public", "ids")],
            );
            sql(
                &engine,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&peer, "CREATE ROLE late_reader; GRANT SELECT ON SEQUENCE ids TO late_reader; INSERT INTO t VALUES (2)");
            let role_oid = peer.durable.roles.read()["late_reader"].oid;
            for subject in [Value::Str("late_reader".into()), Value::Int(role_oid)] {
                for target in [Value::Int(oid), Value::Str("ids".into())] {
                    for column in [false, true] {
                        let context = engine.table_privilege_context();
                        let mut arguments = vec![subject.clone(), target.clone()];
                        if column {
                            arguments.push(Value::Int(1));
                        }
                        arguments.push(Value::Str("SELECT".into()));
                        let result = if column {
                            context.has_column_privilege_value(&arguments)
                        } else {
                            context.has_table_privilege_value(&arguments)
                        }
                        .unwrap();
                        assert_eq!(
                            result,
                            Value::Bool(true),
                            "{provider}: {isolation}: {arguments:?}"
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
fn relation_inquiries_preserve_private_roles_and_sequence_acls_through_savepoints() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(
                &engine,
                "CREATE ROLE reader; CREATE SEQUENCE ids; CREATE TEMP SEQUENCE temporary_ids",
            );
            sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private_acl; CREATE ROLE private_reader; CREATE SEQUENCE private_ids; GRANT SELECT ON SEQUENCE ids, private_ids, temporary_ids TO private_reader"));
            sql(&peer, "CREATE ROLE peer_role; INSERT INTO t VALUES (2)");
            for name in ["ids", "private_ids", "temporary_ids"] {
                let result = sql(&engine, &format!("SELECT has_table_privilege('private_reader', '{name}', 'SELECT') AS t, has_column_privilege('private_reader', '{name}', 'last_value', 'SELECT') AS c"));
                assert_eq!(
                    result.rows[0]["t"],
                    Value::Bool(true),
                    "{provider}: {isolation}: {name}"
                );
                assert_eq!(
                    result.rows[0]["c"],
                    Value::Bool(true),
                    "{provider}: {isolation}: {name}"
                );
            }
            let private_oid = uqa_execution::catalog::projection::sequence_relation_oid(
                engine.durable.sequence_object_ids.read()
                    [&RelationIdentity::new("public", "private_ids")],
            );
            sql(&engine, "ROLLBACK TO private_acl");
            for name in ["ids", "temporary_ids"] {
                let result = sql(&engine, &format!("SELECT has_table_privilege('reader', '{name}', 'SELECT') AS t, has_column_privilege('reader', '{name}', 'last_value', 'SELECT') AS c"));
                assert_eq!(result.rows[0]["t"], Value::Bool(false));
                assert_eq!(result.rows[0]["c"], Value::Bool(false));
            }
            let context = engine.table_privilege_context();
            assert_eq!(
                context
                    .has_table_privilege_value(&[
                        Value::Str("private_reader".into()),
                        Value::Str("ids".into()),
                        Value::Str("SELECT".into()),
                    ])
                    .unwrap_err()
                    .sqlstate(),
                Some("42704")
            );
            assert_eq!(
                context
                    .has_column_privilege_value(&[
                        Value::Str("reader".into()),
                        Value::Int(private_oid),
                        Value::Int(1),
                        Value::Str("SELECT".into()),
                    ])
                    .unwrap(),
                Value::Null
            );
            assert_eq!(
                sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
            );
            sql(&engine, "ROLLBACK");
        }
    }
}
