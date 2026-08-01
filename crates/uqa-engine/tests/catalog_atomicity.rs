//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use uqa_core::{Value, Vertex};
use uqa_engine::Engine;
use uqa_graph::GraphStore as _;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};
use uqa_storage::document_store::Document;
use uqa_storage::{
    Catalog, CatalogFacade, ManagedConnection, PersistentStorageBackend, SQLiteStorageBackend,
};

fn persistent_engine() -> (TempDir, ManagedConnection, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(SQLiteStorageBackend::new(connection.clone()));
    let engine = Engine::from_persistent_backends(catalog, backend).unwrap();
    (dir, connection, engine)
}

fn fail_event(connection: &ManagedConnection, table: &str, event: &str) {
    connection
        .with(|conn| {
            conn.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS injected_catalog_failure;
                 CREATE TRIGGER injected_catalog_failure
                 BEFORE {event} ON {table}
                 BEGIN SELECT RAISE(FAIL, 'injected catalog failure'); END;"
            ))?;
            Ok(())
        })
        .unwrap();
}

fn clear_failure(connection: &ManagedConnection) {
    connection
        .with(|conn| {
            conn.execute_batch("DROP TRIGGER IF EXISTS injected_catalog_failure")?;
            Ok(())
        })
        .unwrap();
}

#[path = "catalog_atomicity/catalog_objects.rs"]
mod catalog_objects;

#[path = "catalog_atomicity/registry_atomicity.rs"]
mod registry_atomicity;

#[path = "catalog_atomicity/schema_graph_atomicity.rs"]
mod schema_graph_atomicity;

#[path = "catalog_atomicity/document_atomicity.rs"]
mod document_atomicity;

#[path = "catalog_atomicity/direct_schema_index_atomicity.rs"]
mod direct_schema_index_atomicity;
