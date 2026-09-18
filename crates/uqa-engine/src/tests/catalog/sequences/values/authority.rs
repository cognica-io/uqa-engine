//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Commits between sequence name resolution and value reads cannot split authorization metadata.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::cell::RefCell;
use uqa_core::{RelationIdentity, Value};
use uqa_execution::catalog::sequence::snapshot::{SequenceReadSnapshot, SequenceSnapshotSource};
use uqa_sql::{
    catalog::{
        resolution::RelationResolution, security::sequence_inquiry::SequencePrivilegeResolution,
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

struct AfterResolution<'a> {
    engine: &'a Engine,
    publish: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

impl SequencePrivilegeResolution for AfterResolution<'_> {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        let resolved = SequencePrivilegeResolution::visible_relation_kind(self.engine, reference)?;
        if let Some(publish) = self.publish.borrow_mut().take() {
            publish();
        }
        Ok(resolved)
    }

    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        SequencePrivilegeResolution::sequence_privilege_oid(self.engine, oid)
    }
}

#[test]
fn sequence_value_authority_uses_roles_and_acl_from_the_same_committed_view() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(&engine, "CREATE ROLE reader; CREATE SEQUENCE ids; GRANT SELECT ON t TO reader; SET ROLE reader");
            sql(
                &engine,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            let resolution = AfterResolution {
                engine: &engine,
                publish: RefCell::new(Some(Box::new(|| {
                    sql(&peer, "BEGIN; CREATE ROLE late_grantee; GRANT USAGE ON SEQUENCE ids TO late_grantee; GRANT late_grantee TO reader; INSERT INTO t VALUES (2); COMMIT");
                }))),
            };
            let mut values = engine.sequence_value_context();
            values.privileges.resolution = &resolution;
            assert_eq!(values.nextval("ids").unwrap(), 1, "{provider}: {isolation}");
            assert_eq!(
                sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 }),
            );
            sql(&engine, "ROLLBACK");
            assert_eq!(peer.nextval("ids").unwrap(), 2);
        }
    }
}

#[test]
fn cached_values_and_introspection_observe_committed_membership_revocation() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(&engine, "CREATE ROLE reader; CREATE ROLE readers; GRANT readers TO reader; CREATE SEQUENCE ids CACHE 5; GRANT ALL ON SEQUENCE ids TO readers; GRANT SELECT ON t TO reader; SET ROLE reader; SELECT nextval('ids')");
            let oid = uqa_execution::catalog::projection::sequence_relation_oid(
                engine.durable.sequence_object_ids.read()[&RelationIdentity::new("public", "ids")],
            );
            sql(
                &engine,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&peer, "REVOKE readers FROM reader");
            let values = engine.sequence_value_context();
            // These functions do not resolve a name or synchronize the live role registry first.
            assert_eq!(
                values.lastval().unwrap_err().into_sql_error().sqlstate(),
                Some("42501")
            );
            let inspect = engine.sequence_introspection_context();
            assert_eq!(
                inspect
                    .pg_sequence_parameters_value(&[Value::Int(oid)])
                    .unwrap_err()
                    .sqlstate(),
                Some("42501")
            );
            assert_eq!(
                inspect
                    .pg_sequence_last_value_value(&[Value::Int(oid)])
                    .unwrap(),
                Value::Null
            );
            assert_eq!(
                inspect
                    .pg_get_sequence_data_value(&[Value::Int(oid)])
                    .unwrap(),
                Value::Record(vec![
                    ("last_value".into(), Value::Null),
                    ("is_called".into(), Value::Null)
                ])
            );
            assert_eq!(
                values
                    .currval("ids")
                    .unwrap_err()
                    .into_sql_error()
                    .sqlstate(),
                Some("42501")
            );
            assert_eq!(
                values
                    .nextval("ids")
                    .unwrap_err()
                    .into_sql_error()
                    .sqlstate(),
                Some("42501")
            );
            assert_eq!(
                values
                    .setval("ids", 40, true)
                    .unwrap_err()
                    .into_sql_error()
                    .sqlstate(),
                Some("42501")
            );
            sql(&engine, "ROLLBACK");
            assert_eq!(peer.nextval("ids").unwrap(), 6);
        }
    }
}

struct AfterSnapshot<'a> {
    engine: &'a Engine,
    publish: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
}

impl SequenceSnapshotSource for AfterSnapshot<'_> {
    fn sequence_read_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot> {
        let snapshot = self.engine.sequence_read_snapshot()?;
        if let Some(publish) = self.publish.borrow_mut().take() {
            publish();
        }
        Ok(snapshot)
    }
}

#[test]
fn retained_value_authority_is_not_replaced_by_a_later_membership_commit() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE ROLE readers; GRANT readers TO reader; CREATE SEQUENCE ids; GRANT USAGE ON SEQUENCE ids TO readers; SET ROLE reader; SELECT nextval('ids')");
        let source = AfterSnapshot {
            engine: &engine,
            publish: RefCell::new(Some(Box::new(|| {
                sql(&peer, "REVOKE readers FROM reader");
            }))),
        };
        let mut values = engine.sequence_value_context();
        values.snapshots = &source;
        assert_eq!(values.lastval().unwrap(), 1);
        assert_eq!(
            values.lastval().unwrap_err().into_sql_error().sqlstate(),
            Some("42501")
        );
    }
}

#[test]
fn setval_by_oid_returns_a_committed_value_before_the_live_registry_contains_the_sequence() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE template_ids");
        let mut row = engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_sequence_rows()
            .unwrap()
            .remove(0);
        row.relation = RelationIdentity::new("public", "late_ids");
        row.object_id = [31; 16];
        row.definition_generation = [32; 16];
        let oid = uqa_execution::catalog::projection::sequence_relation_oid(row.object_id);
        let resolution = AfterResolution {
            engine: &engine,
            publish: RefCell::new(Some(Box::new(|| {
                peer.storage
                    .catalog
                    .as_ref()
                    .unwrap()
                    .create_sequence_row(&row)
                    .unwrap();
            }))),
        };
        let mut values = engine.sequence_value_context();
        values.privileges.resolution = &resolution;
        assert_eq!(values.setval(&oid.to_string(), 42, true).unwrap(), 42);
        assert!(!engine.durable.sequences.read().contains_key(&row.relation));
        assert_eq!(peer.nextval("late_ids").unwrap(), 43);
    }
}

#[test]
fn sequence_value_snapshots_preserve_private_roles_sequences_and_temporary_entries() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(&engine, "CREATE ROLE reader");
            sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_private; CREATE ROLE private_readers; GRANT private_readers TO reader; CREATE SEQUENCE private_ids; CREATE TEMP SEQUENCE temp_ids; GRANT ALL ON SEQUENCE private_ids, temp_ids TO private_readers; SET ROLE reader"));
            sql(&peer, "CREATE ROLE peer_role; CREATE SEQUENCE peer_ids; GRANT USAGE ON SEQUENCE peer_ids TO reader");
            assert_eq!(engine.nextval("private_ids").unwrap(), 1);
            assert_eq!(engine.nextval("temp_ids").unwrap(), 1);
            assert_eq!(engine.nextval("peer_ids").unwrap(), 1);
            sql(&engine, "ROLLBACK TO before_private");
            assert!(engine.nextval("private_ids").is_err());
            assert!(engine.nextval("temp_ids").is_err());
            assert_eq!(engine.nextval("peer_ids").unwrap(), 2);
            sql(&engine, "ROLLBACK");
            assert_eq!(peer.nextval("peer_ids").unwrap(), 3);
        }
    }
}

#[test]
fn direct_sequence_calls_inside_callbacks_keep_the_outer_statement_snapshot() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            let engine = std::sync::Arc::new(engine);
            let peer = std::sync::Arc::new(peer);
            sql(&engine, "CREATE SEQUENCE callback_ids");
            let callback_engine = std::sync::Arc::downgrade(&engine);
            let callback_peer = std::sync::Arc::clone(&peer);
            engine
                .register_scalar_function("direct_sequence_value", move |_: &[Value]| {
                    let engine = callback_engine.upgrade().unwrap();
                    callback_peer.sql("INSERT INTO t VALUES (2)", &[])?;
                    let value = engine.nextval("callback_ids").map_err(SQLError::Internal)?;
                    let rows = engine.sql("SELECT count(*) AS n FROM t", &[])?;
                    assert_eq!(rows.rows[0]["n"], Value::Int(1));
                    Ok(Value::Int(value))
                })
                .unwrap();
            sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}"));
            assert_eq!(
                sql(&engine, "SELECT direct_sequence_value() AS n").rows[0]["n"],
                Value::Int(1)
            );
            assert_eq!(
                sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
            );
            sql(&engine, "ROLLBACK");
            assert_eq!(peer.nextval("callback_ids").unwrap(), 2);
        }
    }
}

#[test]
fn direct_sequence_calls_reject_failed_transactions_before_allocating() {
    let engine = Engine::new();
    sql(&engine, "CREATE SEQUENCE ids");
    sql(&engine, "BEGIN");
    assert!(engine.sql("SELECT 1 / 0", &[]).is_err());
    assert!(engine
        .nextval("ids")
        .unwrap_err()
        .contains("current transaction is aborted"));
    assert!(engine
        .setval("ids", 42)
        .unwrap_err()
        .contains("current transaction is aborted"));
    sql(&engine, "ROLLBACK");
    assert_eq!(engine.nextval("ids").unwrap(), 1);
}
