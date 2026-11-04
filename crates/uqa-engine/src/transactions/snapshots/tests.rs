//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Engine, EngineSnapshotData, Value};
use std::sync::{mpsc, Arc};
use uqa_core::RelationIdentity;
use uqa_storage::read_control::StorageReadControl;

mod authority;
mod authority_roots;
mod capture;
mod catalogs;
mod comments;
mod database_authority;
mod events;
mod graph_oid_roots;
mod identifiers;
mod idle;
mod resources;
mod role_roots;
mod schema_roots;
mod sequence_values;
mod sequences;
mod server_roots;
mod table_metadata;
mod type_roots;
mod undo_sources;
mod wrapper_roots;

fn sql(engine: &Engine, text: &str) {
    engine.sql(text, &[]).unwrap();
}

fn document(snapshot: &EngineSnapshotData) -> Vec<u8> {
    let source = snapshot.tables[&RelationIdentity::new("public", "docs")]
        .document_store
        .read();
    let mut bytes = Vec::new();
    assert!(source
        .write_image_document(1, &mut bytes, &StorageReadControl::with_limit(1 << 20))
        .unwrap()
        .is_some());
    bytes
}

#[test]
fn transaction_data_retention_releases_live_table_owners_after_replacement_and_closure() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'committed'); CREATE VIEW saved AS SELECT id FROM docs");
    let table = engine.table("docs").unwrap().unwrap();
    let weak = Arc::downgrade(&table);
    let original_id = table.object_id();
    let original_views = engine.durable.views.snapshot();
    drop(table);
    sql(&engine, "BEGIN");
    let committed = engine.session.transactions.lock()[0]
        .data_snapshot
        .as_ref()
        .unwrap()
        .data
        .clone();
    let original = document(&committed);
    sql(&engine, "UPDATE docs SET body=repeat('x',1048576); ALTER TABLE docs ADD COLUMN extra integer; SAVEPOINT private");
    let private = engine.session.transactions.lock()[0].savepoints[0]
        .data_snapshot
        .as_ref()
        .unwrap()
        .data
        .clone();
    assert!(!Arc::ptr_eq(&committed, &private));
    sql(&engine, "DROP VIEW saved; DROP TABLE docs; CREATE TABLE docs(replacement boolean); ROLLBACK TO private; COMMIT");
    assert_eq!(
        engine.table("docs").unwrap().unwrap().object_id(),
        original_id
    );
    assert_eq!(
        engine.table("docs").unwrap().unwrap().columns.read().len(),
        3
    );
    sql(
        &engine,
        "DROP VIEW saved; DROP TABLE docs; CREATE TABLE docs(replacement boolean)",
    );
    assert_ne!(
        engine.table("docs").unwrap().unwrap().object_id(),
        original_id
    );
    drop(engine);
    assert!(weak.upgrade().is_none());
    assert_eq!(document(&committed), original);
    let name = RelationIdentity::new("public", "docs");
    assert_eq!(committed.tables[&name].columns.len(), 2);
    assert_eq!(private.tables[&name].columns.len(), 3);
    assert_eq!(
        private.tables[&name]
            .document_store
            .read()
            .get_field(1, "body")
            .unwrap(),
        Some(Value::Str("x".repeat(1 << 20)))
    );
    assert!(Arc::ptr_eq(&committed.durable.views, &original_views));
    assert!(committed.memory_membership.is_some());
}

#[test]
fn outer_transaction_data_is_readable_while_a_long_callback_holds_private_savepoints() {
    let engine = Arc::new(Engine::new());
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'committed')");
    let (ready, started) = mpsc::sync_channel(0);
    let (release, released) = mpsc::sync_channel(0);
    let writer = engine.clone();
    let worker = std::thread::spawn(move || {
        writer.transaction(|engine| {
            engine.sql("UPDATE docs SET body='private'; SAVEPOINT nested; INSERT INTO docs VALUES(2,'savepoint')", &[])?;
            ready.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
    });
    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        let frames = engine
            .session
            .transactions
            .try_lock()
            .expect("callback does not hold the transaction state lock");
        selected = Some(frames[0].data_snapshot.as_ref().unwrap().data.clone());
    });
    assert_eq!(allocation.bytes_total, 0);
    let selected = selected.unwrap();
    let table = &selected.tables[&RelationIdentity::new("public", "docs")];
    assert_eq!(table.document_store.read().len().unwrap(), 1);
    assert_eq!(
        table.document_store.read().get_field(1, "body").unwrap(),
        Some(Value::Str("committed".into()))
    );
    let image = document(&selected);
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS count FROM docs", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    sql(&engine, "UPDATE docs SET body='later'");
    drop(engine);
    assert_eq!(document(&selected), image);
}
