//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live inputs for view deletion within the existing transaction boundary.
use crate::{
    catalog::view::ViewRegistryState,
    schema::{
        publication::dependencies::CatalogPublicationChanges, removal::RelationRemovalRoutines,
        view_dependencies::ViewDependencyContext,
    },
};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::security::view_ownership::ViewOwnershipContext, SQLError};
use uqa_storage::StorageBackendResult;

pub trait ViewRemovalNames {
    fn relation_kind(&self, name: &str) -> Result<Option<(String, &'static str)>, SQLError>;
}
pub trait ViewRemovalEvents {
    fn rules_depending_on_relations(
        &self,
        names: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>>;
    fn drop_rules_depending_on_relations_inner(&self, names: &[String])
        -> StorageBackendResult<()>;
    fn drop_relation_events_inner(&self, relation: &RelationIdentity) -> StorageBackendResult<()>;
}
pub trait ViewRemovalPublication {
    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<Option<bool>>;
}
pub struct ViewRemovalContext<'a> {
    pub registry: &'a dyn ViewRegistryState,
    pub publication: &'a dyn ViewRemovalPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub names: &'a dyn ViewRemovalNames,
    pub events: &'a dyn ViewRemovalEvents,
    pub routines: &'a dyn RelationRemovalRoutines,
    pub dependencies: ViewDependencyContext<'a>,
    pub ownership: ViewOwnershipContext<'a>,
}
pub trait ViewRemovalTransactions: Sized {
    fn with_view_removal<R>(
        &self,
        operation: impl FnOnce(&Self, &ViewRemovalContext<'_>) -> Result<R, SQLError>,
    ) -> Result<R, SQLError>;
}
