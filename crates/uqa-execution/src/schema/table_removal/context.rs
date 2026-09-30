//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The services that remove a table's own state and the objects the catalog's dependencies remove with it.
use crate::{
    routines::removal::context::RoutineRemovalContext,
    schema::{
        events::context::EventLifecycleContext, sequences::removal::SequenceRemovalContext,
        view_removal::context::ViewRemovalContext,
    },
};
use uqa_core::RelationIdentity;
use uqa_sql::{schema::removal::hierarchy::HierarchyDropCatalog, SQLError};
use uqa_storage::StorageBackendResult;

pub trait TableRemovalCatalog {
    fn contains_relation(&self, relation: &RelationIdentity) -> bool;
}
pub trait TableRemovalPublication {
    fn remove_state(&self, name: &str, relation: &RelationIdentity) -> StorageBackendResult<()>;
    fn prune_constraint_modes(&self) -> Result<(), SQLError>;
}
pub struct TableRemovalContext<'a> {
    pub indexes: crate::schema::indexes::registry::IndexRegistryContext<'a>,
    pub catalog: &'a dyn TableRemovalCatalog,
    pub hierarchy: &'a dyn HierarchyDropCatalog,
    pub publication: &'a dyn TableRemovalPublication,
    pub routines: RoutineRemovalContext<'a>,
    pub events: EventLifecycleContext<'a>,
    pub views: ViewRemovalContext<'a>,
    pub sequences: SequenceRemovalContext<'a>,
}
