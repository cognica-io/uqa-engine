//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_core::{RelationIdentity, Value};

#[test]
fn pinned_foreign_catalog_retains_definitions_and_does_not_fall_back_to_live_entries() {
    let engine = Engine::new();
    engine.sql("CREATE SERVER z_source FOREIGN DATA WRAPPER memory_fdw; CREATE SERVER a_source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(id integer) SERVER z_source",&[]).unwrap();
    let snapshot = engine.capture_statement_read_snapshot().unwrap();
    let query = engine.statement_read_snapshot_engine(&snapshot);
    engine.sql("ALTER FOREIGN TABLE items RENAME TO renamed; CREATE FOREIGN TABLE added(id integer) SERVER a_source; CREATE SERVER new_source FOREIGN DATA WRAPPER memory_fdw",&[]).unwrap();
    assert_eq!(
        query.foreign_table("items").unwrap().unwrap().name,
        "public.items"
    );
    assert_eq!(query.foreign_table_columns("items").unwrap(), vec!["id"]);
    assert!(query.foreign_table("renamed").unwrap().is_none());
    assert!(query.foreign_table("added").unwrap().is_none());
    assert!(query.foreign_server("new_source").unwrap().is_none());
    assert_eq!(
        query.foreign_server("z_source").unwrap().unwrap().fdw_type,
        "memory_fdw"
    );
    assert_eq!(
        query.list_foreign_servers().unwrap(),
        vec!["a_source", "z_source"]
    );
    assert_eq!(query.list_foreign_tables().unwrap(), vec!["public.items"]);
    assert!(engine.foreign_table("items").unwrap().is_none());
    assert_eq!(
        engine.list_foreign_servers().unwrap(),
        vec!["a_source", "new_source", "z_source"]
    );
    assert_eq!(
        engine.list_foreign_tables().unwrap(),
        vec!["public.added", "public.renamed"]
    );
}

#[test]
fn memory_stream_reads_current_rows_and_reports_removed_loaded_data() {
    let engine = Engine::new();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(id integer) SERVER source",&[]).unwrap();
    let rows = |second| {
        vec![
            uqa_fdw::Row::from([("id".into(), Value::Int(1))]),
            uqa_fdw::Row::from([("id".into(), Value::Int(second))]),
        ]
    };
    engine.load_memory_foreign_table("items", rows(2)).unwrap();
    let mut stream = engine
        .scan_foreign_table_stream("items", None, &[], None)
        .unwrap();
    assert_eq!(stream.next().unwrap().unwrap()["id"], Value::Int(1));
    engine.load_memory_foreign_table("items", rows(20)).unwrap();
    assert_eq!(stream.next().unwrap().unwrap()["id"], Value::Int(20));
    engine.extensions.foreign_memory_tables.write().clear();
    assert_eq!(
        stream.next().unwrap().unwrap_err(),
        "Foreign table `public.items` lost its loaded memory data during the scan"
    );
}

#[test]
fn missing_foreign_security_blocks_removal_before_memory_or_durable_publication() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("foreign_drop.db")).unwrap();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(id integer) SERVER source",&[]).unwrap();
    engine
        .load_memory_foreign_table(
            "items",
            vec![uqa_fdw::Row::from([("id".into(), Value::Int(1))])],
        )
        .unwrap();
    let relation = RelationIdentity::new("public", "items");
    let before = engine.durable.foreign_tables.snapshot();
    let security = engine
        .durable
        .foreign_table_security
        .write()
        .remove(&relation)
        .unwrap();
    assert_eq!(
        engine
            .foreign_removal_context()
            .drop_foreign_table_inner("items")
            .unwrap_err(),
        "Foreign table `public.items` has no loaded security metadata"
    );
    assert!(Arc::ptr_eq(
        &before,
        &engine.durable.foreign_tables.snapshot()
    ));
    assert_eq!(
        engine.extensions.foreign_memory_tables.read()[&relation].len(),
        1
    );
    assert_eq!(
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_foreign_tables()
            .unwrap()
            .len(),
        1
    );
    engine
        .durable
        .foreign_table_security
        .write()
        .insert(relation, security);
}

#[test]
fn foreign_drop_checks_owned_sequence_dependents_before_removing_persisted_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign_owned_drop.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(id serial) SERVER source; CREATE VIEW dependent AS SELECT nextval('items_id_seq') AS id",&[]).unwrap();
    let relation = RelationIdentity::new("public", "items");
    engine
        .load_memory_foreign_table(
            "items",
            vec![uqa_fdw::Row::from([("id".into(), Value::Int(1))])],
        )
        .unwrap();
    let error = engine.drop_foreign_table("items").unwrap_err();
    assert!(
        error.contains("owned sequence `public.items_id_seq`"),
        "{error}"
    );
    assert!(error.contains("view public.dependent"), "{error}");
    assert!(engine.foreign_table("items").unwrap().is_some());
    assert_eq!(
        engine.extensions.foreign_memory_tables.read()[&relation].len(),
        1
    );
    assert_eq!(
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_sequence_rows()
            .unwrap()
            .len(),
        1
    );
    engine.sql("DROP VIEW dependent", &[]).unwrap();
    assert!(engine.drop_foreign_table("items").unwrap());
    assert!(!engine.drop_foreign_table("items").unwrap());
    assert!(!engine
        .extensions
        .foreign_memory_tables
        .read()
        .contains_key(&relation));
    assert!(engine.durable.foreign_table_security.read().is_empty());
    assert!(engine.durable.sequences.read().is_empty());
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert!(reopened.list_foreign_tables().unwrap().is_empty());
    assert!(reopened
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_sequence_rows()
        .unwrap()
        .is_empty());
    assert_eq!(reopened.list_foreign_servers().unwrap(), vec!["source"]);
}
