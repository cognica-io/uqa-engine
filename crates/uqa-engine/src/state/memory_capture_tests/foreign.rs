//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::foreign::{capture::CapturedForeignTable, StoredForeignTable};
use uqa_sql::catalog::{
    definition_capture::foreign::CapturedForeignServer, foreign_server::ForeignServerDefinition,
};

fn same_servers(
    saved: &[(String, CapturedForeignServer)],
    source: &BTreeMap<String, ForeignServerDefinition>,
) {
    assert_eq!(saved.len(), source.len());
    for ((name, value), (original_name, original)) in saved.iter().zip(source) {
        assert_eq!(name, original_name);
        assert_eq!(value.name, original.name);
        assert_eq!(value.fdw_type, original.fdw_type);
        assert_eq!(value.metadata, original.metadata);
        assert!(value
            .options
            .iter()
            .map(|(k, v)| (k, v))
            .eq(original.options.iter()));
    }
}

fn same_tables(
    saved: &[(RelationIdentity, CapturedForeignTable)],
    source: &BTreeMap<RelationIdentity, StoredForeignTable>,
) {
    assert_eq!(saved.len(), source.len());
    for ((name, value), (original_name, original)) in saved.iter().zip(source) {
        assert_eq!(name, original_name);
        assert_eq!(value.name, original.name);
        assert_eq!(value.persistence, original.persistence);
        assert_eq!(value.object_id, original.object_id);
        assert_eq!(value.catalog_oids, original.catalog_oids);
        assert_eq!(value.row_type_array_name, original.row_type_array_name);
        assert_eq!(value.server_name, original.server_name);
        assert_eq!(value.server_reference, original.server_reference);
        assert_eq!(value.dropped_attributes, original.dropped_attributes);
        assert_eq!(value.option_order, original.option_order);
        assert!(value
            .options
            .iter()
            .map(|(k, v)| (k, v))
            .eq(original.options.iter()));
        assert_eq!(
            serde_json::to_value(&value.columns).unwrap(),
            serde_json::to_value(&original.columns).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&value.checks).unwrap(),
            serde_json::to_value(&original.checks).unwrap()
        );
    }
}

#[test]
fn memory_foreign_capture_keeps_definitions_through_rollback_replacement_and_unread_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler OPTIONS (z 'first',a 'second'); CREATE SERVER remote TYPE 'kind' VERSION 'v1' FOREIGN DATA WRAPPER custom OPTIONS (z 'first',a 'second'); CREATE FOREIGN TABLE remote_rows(gone integer, payload text DEFAULT 'original', CONSTRAINT valid CHECK(length(payload)>0)) SERVER remote OPTIONS(z 'first',a 'second'); ALTER FOREIGN TABLE remote_rows DROP gone");
        let wrappers = engine.durable.foreign_wrappers.snapshot();
        let servers = engine.durable.foreign_servers.snapshot();
        let tables = engine.durable.foreign_tables.snapshot();
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables.first_key_value().unwrap().1.dropped_attributes.len(),
            1
        );
        let wrapper_key = engine.durable.foreign_wrappers.key().unwrap();
        let server_key = engine.durable.foreign_servers.key().unwrap();
        let table_key = engine.durable.foreign_tables.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; DROP FOREIGN TABLE remote_rows; SAVEPOINT removed; DROP SERVER remote; ROLLBACK TO removed; ROLLBACK");
            sql(&engine, "DROP FOREIGN DATA WRAPPER custom CASCADE; CREATE FOREIGN DATA WRAPPER custom HANDLER memory_fdw_handler; CREATE SERVER remote FOREIGN DATA WRAPPER custom; CREATE FOREIGN TABLE remote_rows(replacement integer) SERVER remote");
            assert_ne!(
                engine.durable.foreign_wrappers.read()["custom"].identity,
                wrappers["custom"].identity
            );
        }
        drop(engine);
        let saved_wrappers = wrapper_key.retained(&cut).unwrap().unwrap();
        let saved_servers = server_key.retained(&cut).unwrap().unwrap();
        let saved_tables = table_key.retained(&cut).unwrap().unwrap();
        assert!(saved_wrappers.get().unwrap().iter().eq(wrappers.iter()));
        same_servers(saved_servers.get().unwrap(), &servers);
        same_tables(saved_tables.get().unwrap(), &tables);
        drop(cut);
        control.cancellation().cancel();
        assert!(saved_wrappers.get().is_err());
        assert!(saved_servers.get().is_err());
        assert!(saved_tables.get().is_err());
        drop((saved_wrappers, saved_servers, saved_tables));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_foreign_capture_keeps_ordinary_ddl_and_independent_cuts_usable() {
    let engine = Engine::new();
    sql(
        &engine,
        &format!(
            "CREATE FOREIGN DATA WRAPPER large OPTIONS (payload '{}')",
            "x".repeat(32768)
        ),
    );
    let key = engine.durable.foreign_wrappers.key().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let denied = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(
        &engine,
        "DROP FOREIGN DATA WRAPPER large; CREATE FOREIGN DATA WRAPPER large",
    );
    assert!(engine.durable.foreign_wrappers.read()["large"]
        .options
        .is_empty());
    assert!(denied.check().is_err());
    let saved = key.retained(&cut).unwrap().unwrap();
    let original = saved.get().unwrap().get("large").unwrap();
    assert_eq!(original.options[0].1.len(), 32768);
    drop((saved, cut, denied, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
