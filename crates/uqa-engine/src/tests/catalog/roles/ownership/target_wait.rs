//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn role_binding_order_matches_relation_and_routine_target_waits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS
                .iter()
                .filter(|target| target.catalog != "pg_namespace")
            {
                let (_directory, first, second) = sessions(provider);
                let third = first.new_session().unwrap();
                sql(&first, "CREATE ROLE dependent");
                sql(&first, target.setup);
                let original = first.durable.roles.read()["dependent"].object_id;
                let key = if let Some(relation) = target.relation {
                    first.row_locks.table_key(&format!("public.{relation}"))
                } else {
                    let value = sql(
                        &first,
                        &format!("SELECT oid FROM pg_proc WHERE {}", target.condition),
                    )
                    .rows[0]["oid"]
                        .clone();
                    let Value::Int(oid) = value else {
                        panic!("routine OID")
                    };
                    first
                        .row_locks
                        .shared_catalog_key(SharedCatalogLock::Object {
                            class_id: 1255,
                            oid: u32::try_from(oid).unwrap(),
                        })
                };
                sql(&first, "BEGIN");
                sql(&first, &target.alter("uqa"));
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
                );
                let session = second.session_id;
                let cancel = second.runtime.cancellation.clone();
                let statement = target.alter("dependent");
                let (sender, result) = mpsc::channel();
                let worker = thread::spawn(move || {
                    let result = second.sql(&statement, &[]);
                    sender.send(result).unwrap();
                    second
                });
                let deadline = Instant::now() + Duration::from_secs(30);
                while !first.row_locks.waiting_for_relation(session, key)
                    && !worker.is_finished()
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(1));
                }
                let waited = first.row_locks.waiting_for_relation(session, key);
                let replaced = third.sql("DROP ROLE dependent; CREATE ROLE dependent", &[]);
                let released = first.sql("COMMIT", &[]);
                if replaced.is_err() || released.is_err() {
                    cancel.cancel();
                }
                let result = result.recv_timeout(Duration::from_secs(30));
                if result.is_err() {
                    cancel.cancel();
                }
                let second = worker.join().unwrap();
                replaced.unwrap();
                released.unwrap();
                assert!(waited, "expected target lock wait for {}", target.object);
                assert_ne!(third.durable.roles.read()["dependent"].object_id, original);
                let changed = target.relation.is_some();
                if changed {
                    result.unwrap().unwrap();
                    sql(&second, "COMMIT");
                } else {
                    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("42704"));
                    sql(&second, "ROLLBACK");
                }
                target.assert_owner(&third, if changed { "dependent" } else { "uqa" });
                assert_eq!(
                    sql(&third, "SELECT v FROM t").rows.len(),
                    if changed { 2 } else { 1 }
                );
            }
        }
    }
}
