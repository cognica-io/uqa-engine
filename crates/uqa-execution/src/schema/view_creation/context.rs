//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View creation inputs and live transaction, binding, and owner scopes.
use super::super::{
    ctas::TableAsQuerySource,
    publication::dependencies::CatalogPublicationChanges,
    view_alteration::{ViewAlterAccess, ViewAlterCatalog},
};
use crate::catalog::view::{StoredView, ViewPublication};
use uqa_sql::{
    catalog::regrole_dependencies::StoredRegroleResolver,
    plan::QueryPlan,
    routines::RoutineResolution,
    schema::{relation_alteration::RelationAlterNames, view_creation::ViewCreationNamespace},
    semantics::view_rewrite::context::ViewRewriteContext,
    RowSchema, SQLError, SQLParam, SQLResult,
};
use uqa_storage::StorageBackendResult;

pub trait ViewCreationCatalog {
    fn synchronize(&self) -> StorageBackendResult<()>;
    fn allocate_identity(&self) -> StorageBackendResult<[u8; 16]>;
}
pub trait ViewPlanBinding {
    fn bind_relations(&self, plan: &mut QueryPlan) -> Result<bool, SQLError>;
    fn bind_routines(
        &self,
        plan: &mut QueryPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError>;
}
pub trait MaterializedViewAccess {
    fn current_user_name(&self) -> String;
    fn target_name(&self, name: &str) -> Result<String, SQLError>;
    fn ensure_create(&self, name: &str) -> Result<(), SQLError>;
    fn ensure_maintenance(&self, name: &str, view: &StoredView) -> Result<(), SQLError>;
}
pub type ViewOwnerQuery<'a> =
    Box<dyn FnOnce(&dyn TableAsQuerySource) -> Result<SQLResult, SQLError> + 'a>;
pub trait ViewQueryOwners {
    fn with_owner(&self, owner: &str, operation: ViewOwnerQuery<'_>)
        -> Result<SQLResult, SQLError>;
}
pub struct ViewCreationContext<'a> {
    pub catalog: &'a dyn ViewCreationCatalog,
    pub views: &'a dyn ViewAlterCatalog,
    pub namespace: &'a dyn ViewCreationNamespace,
    pub names: &'a dyn RelationAlterNames,
    pub owners: &'a dyn ViewAlterAccess,
    pub access: &'a dyn MaterializedViewAccess,
    pub bindings: &'a dyn ViewPlanBinding,
    pub routines: &'a dyn RoutineResolution,
    pub regroles: &'a dyn StoredRegroleResolver,
    pub rewrite: ViewRewriteContext<'a>,
    pub queries: &'a dyn TableAsQuerySource,
    pub query_owners: &'a dyn ViewQueryOwners,
    pub publication: &'a dyn ViewPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub type ViewCreationWrite<'a> =
    Box<dyn FnOnce(&ViewCreationContext<'_>) -> Result<(), SQLError> + 'a>;
pub type MaterializedViewWrite<'a> =
    Box<dyn FnOnce(&ViewCreationContext<'_>) -> Result<Option<u64>, SQLError> + 'a>;
pub trait ViewCreationTransactions {
    fn with_view_creation(&self, write: ViewCreationWrite<'_>) -> Result<(), SQLError>;
    fn with_materialized_view_creation(
        &self,
        write: MaterializedViewWrite<'_>,
    ) -> Result<Option<u64>, SQLError>;
}
