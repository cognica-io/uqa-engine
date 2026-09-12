//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation removal services and the session boundary that supplies fresh catalog inputs.
use super::super::indexes::removal::IndexRemovalContext;
use uqa_sql::{
    schema::removal::{ForeignTableDropDependencies, RelationDropCatalog},
    SQLError, SQLResult,
};
use uqa_storage::StorageBackendResult;

pub trait RelationRemovalPrivileges {
    fn ensure_table_drop_authority(&self, table: &str) -> Result<(), SQLError>;
    fn ensure_foreign_table_drop_authority(&self, table: &str) -> Result<(), SQLError>;
}
pub trait RelationRemovalRoutines {
    fn drop_relation_routine_dependents(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError>;
}
pub trait RelationRemovalEvents {
    fn ensure_no_pending_trigger_events(&self, table: &str, action: &str) -> Result<(), SQLError>;
    fn drop_rules_depending_on_relations_inner(&self, names: &[String])
        -> StorageBackendResult<()>;
}
pub trait RelationRemovalViews {
    fn drop_views(&self, names: &[String], cascade: bool, kind: &str) -> Result<(), SQLError>;
    fn drop_views_depending_on_relations(&self, names: &[String]) -> StorageBackendResult<()>;
}

pub trait RelationRemovalLocks {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError>;
}
pub type RelationRemovalWrite<'a> =
    Box<dyn FnOnce(&RelationRemovalContext<'_>) -> Result<SQLResult, SQLError> + 'a>;
pub trait RelationRemovalTransactions {
    fn with_relation_write(&self, write: RelationRemovalWrite<'_>) -> Result<SQLResult, SQLError>;
}
pub struct RelationRemovalContext<'a> {
    pub catalog: &'a dyn RelationDropCatalog,
    pub dependencies: &'a dyn ForeignTableDropDependencies,
    pub tables: crate::schema::table_removal::context::TableRemovalContext<'a>,
    pub privileges: &'a dyn RelationRemovalPrivileges,
    pub routines: &'a dyn RelationRemovalRoutines,
    pub events: &'a dyn RelationRemovalEvents,
    pub foreign_tables: super::super::foreign_removal::ForeignTableRemovalContext<'a>,
    pub views: &'a dyn RelationRemovalViews,
    pub sequences: &'a dyn crate::schema::sequences::removal::SequenceRemovalInputs,
    pub locks: &'a dyn RelationRemovalLocks,
    pub transactions: &'a dyn RelationRemovalTransactions,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
    pub indexes: IndexRemovalContext<'a>,
}
