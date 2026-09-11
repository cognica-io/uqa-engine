//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live registry and catalog inputs for durable view restoration.
use super::super::{StoredView, ViewIdentityAllocation, ViewRegistryState};
use crate::{
    catalog::foreign::StoredForeignTable, schema::view_creation::context::ViewPlanBinding,
};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_graph::GraphStoreHandle;
use uqa_sql::{catalog::roles::guards::RoleCatalogGuards, RowSchema, SQLError};
use uqa_storage::{CatalogFacade, StorageBackendResult, ViewRow};

pub trait ViewRowsStorage {
    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>>;
    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()>;
}
pub struct CatalogViewRows<'a> {
    pub catalog: &'a dyn CatalogFacade,
}
impl ViewRowsStorage for CatalogViewRows<'_> {
    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        self.catalog.load_views()
    }
    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        self.catalog.save_view(view)
    }
}
pub trait ViewRestoreTableNames {
    fn names(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_>;
}
pub type ViewRestoreForeignRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type ViewRestoreGraphsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<String, Arc<GraphStoreHandle>>> + 'a>;
pub trait ViewRestoreNamespace {
    fn tables(&self) -> Box<dyn ViewRestoreTableNames + '_>;
    fn foreign_tables(&self) -> ViewRestoreForeignRead<'_>;
    fn graphs(&self) -> ViewRestoreGraphsRead<'_>;
}
pub trait ViewRestoreSchemas {
    fn stored_schema(&self, view: &StoredView) -> Result<RowSchema, SQLError>;
}
pub trait ViewRestoreSequences {
    fn resolve_loaded(&self, reference: &str) -> StorageBackendResult<String>;
}
pub struct ViewRestoreContext<'a> {
    pub registry: &'a dyn ViewRegistryState,
    pub namespace: &'a dyn ViewRestoreNamespace,
    pub roles: &'a dyn RoleCatalogGuards,
    pub identities: &'a dyn ViewIdentityAllocation,
    pub bindings: &'a dyn ViewPlanBinding,
    pub schemas: &'a dyn ViewRestoreSchemas,
    pub sequences: &'a dyn ViewRestoreSequences,
}
