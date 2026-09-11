//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Separate catalog, dependency, and publication services for routine removal.

use crate::{
    catalog::{
        context::CatalogContext, foreign::StoredForeignTable, security::roles::RoleCatalogGuards,
    },
    schema::{domains::dependencies::DomainDependencyContext, namespaces::NamespaceCatalogChanges},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, CreateFunction, DropRule, DropTrigger, FunctionBinding, TableCheck},
    routines::lifecycle::{
        names::RoutineNameCatalog, relations::RoutineColumnBinding, RoutineRegistry,
    },
    schema::sequences::dependents::SequenceSchemaDependent,
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type RoutineRegistryWrite<'a> = Box<dyn DerefMut<Target = RoutineRegistry> + 'a>;
pub trait RoutineRegistryState {
    fn routine_snapshot(&self) -> RoutineRegistry;
    fn routines_write(&self) -> RoutineRegistryWrite<'_>;
}
pub trait RoutineRegistryPublication {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError>;
}
pub type RoutineColumnRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub type RoutineCheckRead<'a> = Box<dyn Deref<Target = Vec<TableCheck>> + 'a>;
pub trait RoutineTableMetadata {
    fn object_id(&self) -> [u8; 16];
    fn columns(&self) -> RoutineColumnRead<'_>;
    fn table_checks(&self) -> RoutineCheckRead<'_>;
}
pub type RoutineForeignRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub trait RoutineDependencyCatalog {
    fn routine_table_schemas(&self) -> Vec<(String, Arc<dyn RoutineTableMetadata>)>;
    fn routine_foreign_tables(&self) -> RoutineForeignRead<'_>;
}
pub trait RoutineViewDependencies {
    fn views_depending_on_function(
        &self,
        target: &FunctionBinding,
    ) -> StorageBackendResult<Vec<String>>;
    fn views_depending_on_relation(&self, relation: &str) -> StorageBackendResult<Vec<String>>;
    fn views_depending_on_sequence(&self, sequence: &str) -> StorageBackendResult<Vec<String>>;
}
pub trait RoutineEventDependencies {
    fn triggers_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<(String, String)>, SQLError>;
    fn rules_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>>;
    fn rules_depending_on_relations(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>>;
}
pub trait RoutineIndexDependencies {
    fn indexes_depending_on_routine(
        &self,
        target: &FunctionBinding,
    ) -> Result<Vec<RelationIdentity>, SQLError>;
}
pub trait RoutineSequenceDependencies {
    fn sequence_names(&self) -> Vec<String>;
    fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>>;
    fn sequence_names_owned_by_tables(
        &self,
        owners: &BTreeSet<[u8; 16]>,
    ) -> StorageBackendResult<BTreeSet<String>>;
    fn sequence_names_owned_by_column(
        &self,
        table: [u8; 16],
        column: [u8; 16],
    ) -> StorageBackendResult<BTreeSet<String>>;
}
pub struct RoutineDependencyContext<'a> {
    pub catalog: &'a dyn RoutineDependencyCatalog,
    pub views: &'a dyn RoutineViewDependencies,
    pub events: &'a dyn RoutineEventDependencies,
    pub indexes: &'a dyn RoutineIndexDependencies,
    pub sequences: &'a dyn RoutineSequenceDependencies,
    pub columns: &'a dyn RoutineColumnBinding,
}
pub trait RoutineBodyRewrites {
    fn prepare_routine_column_alias_drop(
        &self,
        columns: BTreeSet<(String, String)>,
        removed: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError>;
    fn publish_stored_routine_body_rewrites(
        &self,
        definitions: Vec<CreateFunction>,
    ) -> Result<(), SQLError>;
    fn refresh_stored_merge_target_plans(&self) -> Result<(), SQLError>;
}
pub trait RoutineTableRemoval {
    fn set_column_default_none(&self, table: &str, column: &str) -> StorageBackendResult<bool>;
    fn try_drop_column_inner(&self, table: &str, column: &str) -> StorageBackendResult<bool>;
}
pub trait RoutineForeignRemoval {
    fn drop_foreign_table_check_dependency(
        &self,
        table: &str,
        constraint: &str,
    ) -> StorageBackendResult<Option<bool>>;
    fn clear_foreign_table_default_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<bool>>;
    fn drop_foreign_table_generated_column_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<bool>>;
}
pub trait RoutineEventRemoval {
    fn drop_rule(&self, statement: &DropRule) -> Result<(), SQLError>;
    fn drop_trigger(&self, statement: &DropTrigger) -> Result<(), SQLError>;
}
pub trait RoutineDropNotices {
    fn routine_drop_notice(&self, level: &str, message: &str);
}
pub struct RoutineRemovalContext<'a> {
    pub names: &'a dyn RoutineNameCatalog,
    pub registry: &'a dyn RoutineRegistryState,
    pub publication: &'a dyn RoutineRegistryPublication,
    pub roles: &'a dyn RoleCatalogGuards,
    pub catalog: CatalogContext<'a>,
    pub domains: DomainDependencyContext<'a>,
    pub dependencies: RoutineDependencyContext<'a>,
    pub bodies: &'a dyn RoutineBodyRewrites,
    pub tables: &'a dyn RoutineTableRemoval,
    pub foreign: &'a dyn RoutineForeignRemoval,
    pub events: &'a dyn RoutineEventRemoval,
    pub notices: &'a dyn RoutineDropNotices,
    pub changes: &'a dyn NamespaceCatalogChanges,
}
