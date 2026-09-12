//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_core::RelationIdentity;

#[test]
fn missing_security_metadata_prevents_foreign_definition_publication() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("foreign_definition.db")).unwrap();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(id integer DEFAULT 7) SERVER source",&[]).unwrap();
    let relation = RelationIdentity::from_legacy_name("public.items").unwrap();
    let before = engine.durable.foreign_tables.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let stored = catalog.load_foreign_tables().unwrap()[0]
        .columns_json
        .clone();
    let security = engine
        .durable
        .foreign_table_security
        .write()
        .remove(&relation)
        .unwrap();
    let error = engine
        .foreign_definition_context()
        .clear_foreign_table_default_dependency("public.items", "id")
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("foreign table `public.items` has no security metadata"));
    assert!(Arc::ptr_eq(
        &before,
        &engine.durable.foreign_tables.snapshot()
    ));
    assert_eq!(
        catalog.load_foreign_tables().unwrap()[0].columns_json,
        stored
    );
    engine
        .durable
        .foreign_table_security
        .write()
        .insert(relation, security);
}

#[test]
fn foreign_column_removal_persists_acl_cleanup_and_retained_column_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign_column.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE ROLE reader; CREATE FOREIGN TABLE items(id integer, kept integer CHECK (kept > 0), CONSTRAINT dependent CHECK (id > 0), CONSTRAINT keep_check CHECK (kept < 100)) SERVER source; GRANT SELECT(id) ON items TO reader",&[]).unwrap();
    let relation = RelationIdentity::from_legacy_name("public.items").unwrap();
    let kept_id = engine.durable.foreign_tables.read()[&relation].columns[1].object_id;
    assert!(kept_id.is_some());
    assert!(engine.durable.foreign_table_security.read()[&relation]
        .column_acls
        .contains_key("id"));
    assert_eq!(
        engine
            .with_implicit_storage_transaction(|engine| engine
                .foreign_definition_context()
                .drop_foreign_table_column_dependency("public.items", "id"))
            .unwrap(),
        Some(true)
    );
    let stored = engine
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_foreign_tables()
        .unwrap()[0]
        .columns_json
        .clone();
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    let tables = reopened.durable.foreign_tables.read();
    let table = &tables[&relation];
    assert_eq!(table.columns.len(), 1);
    assert_eq!(table.columns[0].name, "kept");
    assert_eq!(table.columns[0].object_id, kept_id);
    assert!(table.columns[0].check.is_some());
    assert_eq!(table.checks.len(), 1);
    assert_eq!(table.checks[0].name.as_deref(), Some("keep_check"));
    assert!(!reopened.durable.foreign_table_security.read()[&relation]
        .column_acls
        .contains_key("id"));
    assert_eq!(
        reopened
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_foreign_tables()
            .unwrap()[0]
            .columns_json,
        stored
    );
}
