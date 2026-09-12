//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained ordinary-table generations and native dependency inputs for DROP.
use crate::{
    routines::removal::context::RoutineRemovalContext,
    schema::{
        events::context::EventLifecycleContext, sequences::removal::SequenceRemovalContext,
        view_removal::context::ViewRemovalContext,
    },
};
use std::ops::DerefMut;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    schema::removal::{hierarchy::HierarchyDropCatalog, tables::TableRemovalMetadata},
    SQLError,
};
use uqa_storage::StorageBackendResult;
pub type TableColumnsWrite<'a> = Box<dyn DerefMut<Target = Vec<ColumnDef>> + 'a>;
pub type TableChecksWrite<'a> = Box<dyn DerefMut<Target = Vec<TableCheck>> + 'a>;
pub type TableForeignKeysWrite<'a> = Box<dyn DerefMut<Target = Vec<ForeignKey>> + 'a>;
pub trait TableRemovalState: TableRemovalMetadata {
    fn object_id(&self) -> [u8; 16];
    fn columns_write(&self) -> TableColumnsWrite<'_>;
    fn checks_write(&self) -> TableChecksWrite<'_>;
    fn foreign_keys_write(&self) -> TableForeignKeysWrite<'_>;
    fn persist_constraints(
        &self,
        columns: &[ColumnDef],
        checks: &[TableCheck],
        foreign_keys: &[ForeignKey],
        keys: &[TableKeyConstraint],
    ) -> StorageBackendResult<()>;
}
pub type TableRemovalEntry<'a> = (String, Box<dyn TableRemovalState + 'a>);
pub type TableDropCandidate<'a> = (
    String,
    Box<dyn TableRemovalState + 'a>,
    Vec<ColumnDef>,
    Vec<TableCheck>,
    Vec<ForeignKey>,
    Vec<TableKeyConstraint>,
);
pub trait TableRemovalCatalog {
    fn relation_kind(&self, name: &str) -> StorageBackendResult<Option<(String, &'static str)>>;
    fn table_entries(&self) -> Vec<TableRemovalEntry<'_>>;
    fn contains_relation(&self, relation: &RelationIdentity) -> bool;
}
pub trait TableRemovalPublication {
    fn remove_state(&self, name: &str, relation: &RelationIdentity) -> StorageBackendResult<()>;
    fn prune_constraint_modes(&self) -> Result<(), SQLError>;
}
pub type TableRemovalWrite<'a> =
    Box<dyn FnOnce(&TableRemovalContext<'_>) -> StorageBackendResult<()> + 'a>;
pub trait TableRemovalTransactions {
    fn with_table_removal_write(&self, write: TableRemovalWrite<'_>) -> StorageBackendResult<()>;
}
pub struct TableRemovalContext<'a> {
    pub catalog: &'a dyn TableRemovalCatalog,
    pub hierarchy: &'a dyn HierarchyDropCatalog,
    pub publication: &'a dyn TableRemovalPublication,
    pub transactions: &'a dyn TableRemovalTransactions,
    pub routines: RoutineRemovalContext<'a>,
    pub events: EventLifecycleContext<'a>,
    pub views: ViewRemovalContext<'a>,
    pub sequences: SequenceRemovalContext<'a>,
}
