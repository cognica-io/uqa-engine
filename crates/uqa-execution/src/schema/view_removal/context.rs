//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live inputs for view removal within the existing transaction boundary.
use crate::{
    catalog::view::ViewRegistryState, schema::publication::dependencies::CatalogPublicationChanges,
};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::security::view_ownership::ViewOwnershipContext;
use uqa_storage::StorageBackendResult;

pub trait ViewRemovalEvents {
    fn drop_relation_events_inner(&self, relation: &RelationIdentity) -> StorageBackendResult<()>;
}
pub trait ViewRemovalPublication {
    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<Option<bool>>;
}
pub struct ViewRemovalContext<'a> {
    pub registry: &'a dyn ViewRegistryState,
    pub publication: &'a dyn ViewRemovalPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub events: &'a dyn ViewRemovalEvents,
    pub ownership: ViewOwnershipContext<'a>,
}
