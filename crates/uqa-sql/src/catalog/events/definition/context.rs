//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition analysis over live event metadata and fresh catalog binding scopes.

use crate::{
    ast::{ColumnDef, TableHierarchy},
    binding::{
        stored_columns::StoredColumnBindingContext,
        stored_relations::{StoredQuerySequences, StoredRelationCatalog},
        stored_routines::analysis::CatalogRoutineAnalysisContext,
    },
    catalog::{
        regrole_dependencies::StoredRegroleResolver, security::table::TableAclPrivilege,
        stored_view::StoredView, view::StoredViewKind,
    },
    routines::{
        compilation::RoutineCompilationCatalog, security::RoutineExecutionAuthority,
        RoutineResolution,
    },
    semantics::{
        mutation_privileges::MutationPrivilegeCatalog, returning::ReturningAnalysisContext,
        rules::action_binding::RuleSourceCatalog,
    },
    RowSchema, SQLError,
};
use uqa_core::RelationIdentity;

/// Read the current catalog entry at the point required by definition analysis.
pub trait EventRelationCatalog {
    fn event_relation_owner(
        &self,
        relation: &RelationIdentity,
    ) -> Result<(String, &'static str), SQLError>;
    fn view_kind(&self, relation: &RelationIdentity) -> Option<StoredViewKind>;
    fn view(&self, relation: &RelationIdentity) -> Option<StoredView>;
    fn foreign_columns(&self, relation: &RelationIdentity) -> Option<Vec<ColumnDef>>;
    fn restored_catalog_view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError>;
    fn stored_view_schema(&self, view: &StoredView) -> Result<RowSchema, SQLError>;
    fn loaded_table_hierarchy(&self, relation: &RelationIdentity) -> Option<TableHierarchy>;
}
/// Foreign-table authorization retains its own role and security read guards.
pub trait EventForeignPrivileges {
    fn ensure_foreign_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct EventAnalysisContext<'a> {
    pub catalog: &'a dyn EventRelationCatalog,
    pub relations: &'a dyn StoredRelationCatalog,
    pub sources: &'a dyn RuleSourceCatalog,
    pub routines: &'a dyn RoutineResolution,
    pub authority: &'a dyn RoutineExecutionAuthority,
    pub privileges: &'a dyn MutationPrivilegeCatalog,
    pub foreign_privileges: &'a dyn EventForeignPrivileges,
    pub columns: StoredColumnBindingContext<'a>,
    pub returning: ReturningAnalysisContext<'a>,
    pub stored_routines: CatalogRoutineAnalysisContext<'a>,
    pub namespaces: &'a dyn RoutineCompilationCatalog,
    pub sequences: &'a dyn StoredQuerySequences,
    pub regroles: &'a dyn StoredRegroleResolver,
}
