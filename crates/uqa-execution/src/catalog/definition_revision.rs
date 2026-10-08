//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation definition identity is independent of representation-only stored expression rewrites.

use uqa_storage::{CatalogFacade, StorageBackendResult};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TablePublicationKind {
    Definition,
    ExpressionNames,
}

pub fn table_key(object: [u8; 16]) -> String {
    format!(
        "__uqa_prepared_table_definition_{:032x}",
        u128::from_be_bytes(object)
    )
}

/// Every definition publication gets a fresh record identity, even when its payload equals the previous definition. Missing legacy markers are a stable initial identity until the first real definition change.
pub fn publish_table(catalog: &dyn CatalogFacade, object: [u8; 16]) -> StorageBackendResult<()> {
    publish(catalog, &table_key(object))
}

fn publish(catalog: &dyn CatalogFacade, key: &str) -> StorageBackendResult<()> {
    let revision = super::identity::new_nonzero_catalog_identity("relation", "analysis revision")?;
    catalog.set_metadata(key, &format!("{:032x}", u128::from_be_bytes(revision)))
}

pub fn remove_table(catalog: &dyn CatalogFacade, object: [u8; 16]) -> StorageBackendResult<()> {
    catalog.delete_metadata(&table_key(object))
}

pub fn view_key(object: [u8; 16]) -> String {
    format!(
        "__uqa_prepared_view_definition_{:032x}",
        u128::from_be_bytes(object)
    )
}

pub fn publish_view(
    catalog: &dyn CatalogFacade,
    row: &uqa_storage::ViewRow,
) -> StorageBackendResult<()> {
    #[derive(serde::Deserialize)]
    struct Identity {
        object_id: [u8; 16],
    }
    let identity: Identity = serde_json::from_str(&row.definition_json)?;
    catalog.save_view(row)?;
    publish(catalog, &view_key(identity.object_id))
}

pub fn remove_view(catalog: &dyn CatalogFacade, object: [u8; 16]) -> StorageBackendResult<()> {
    catalog.delete_metadata(&view_key(object))
}
