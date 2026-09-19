//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent membership tuples follow captured endpoint identities and shared-role lock schedules.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};

mod commands;
mod deletion;
mod endpoints;
mod locks;

pub(super) fn before_holder_ends(holder: &Engine, worker: Engine, statement: &str) -> Engine {
    use std::{sync::mpsc, thread, time::Duration};
    let statement = statement.to_string();
    let cancel = worker.runtime.cancellation.clone();
    let (send, receive) = mpsc::channel();
    let task = thread::spawn(move || {
        let _ = send.send(worker.sql(&statement, &[]));
        worker
    });
    let result = receive.recv_timeout(Duration::from_secs(30));
    if result.is_err() {
        cancel.cancel();
        sql(holder, "ROLLBACK");
    }
    let worker = task.join().unwrap();
    result
        .expect("independent catalog command must finish before the holder ends")
        .unwrap();
    worker
}

fn role_lock(engine: &Engine, name: &str) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()[name].oid).unwrap(),
    }
}

fn membership_rows(engine: &Engine) -> Vec<uqa_sql::ResultRow> {
    sql(engine, "SELECT oid, roleid, member, grantor, admin_option, inherit_option, set_option FROM pg_auth_members ORDER BY oid").rows
}
