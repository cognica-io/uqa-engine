//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub use uqa_sql::catalog::{stored_view::StoredView, view::StoredViewKind};

use uqa_core::RelationIdentity;
use uqa_storage::ViewRow;

pub fn catalog_view_row(
    relation: &RelationIdentity,
    view: &StoredView,
) -> Result<ViewRow, serde_json::Error> {
    Ok(ViewRow {
        relation: relation.clone(),
        role_owner: view.role_owner.clone(),
        acl: view.acl.clone(),
        column_acls: view.column_acls.clone(),
        definition_json: serde_json::to_string(view)?,
    })
}

/// Actual retained registry guard; publication ordering is controlled by the executor.
pub type ViewRegistryWrite<'a> = Box<
    dyn std::ops::DerefMut<Target = std::collections::BTreeMap<RelationIdentity, StoredView>> + 'a,
>;

pub type ViewRegistryRead<'a> = Box<
    dyn std::ops::Deref<Target = std::collections::BTreeMap<RelationIdentity, StoredView>> + 'a,
>;

pub trait ViewRegistryState {
    fn views_read(&self) -> ViewRegistryRead<'_>;
    fn views_write(&self) -> ViewRegistryWrite<'_>;
}
pub trait ViewIdentityAllocation {
    fn allocate_identity(&self) -> uqa_storage::StorageBackendResult<[u8; 16]>;
}
pub trait ViewPublication: ViewRegistryState {
    fn has_catalog(&self) -> bool;
    fn save_view(&self, row: &ViewRow) -> uqa_storage::StorageBackendResult<()>;
}

pub mod restoration;
