//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation removal services and the session boundary that supplies fresh catalog inputs.
use super::super::indexes::removal::IndexRemovalContext;
use crate::row_locks::binding::{RelationDefinitionSession, RelationLockCatalog};
use uqa_sql::{schema::removal::RelationDropCatalog, SQLError, SQLResult};

pub trait RelationRemovalPrivileges {
    fn ensure_table_drop_authority(&self, table: &str) -> Result<(), SQLError>;
    fn ensure_foreign_table_drop_authority(&self, table: &str) -> Result<(), SQLError>;
}
pub trait RelationRemovalEvents {
    fn ensure_no_pending_trigger_events(&self, table: &str, action: &str) -> Result<(), SQLError>;
}
pub trait RelationRemovalViews {
    fn lock_dependent_views(&self, names: &[String]) -> Result<(), SQLError>;
    fn ensure_view_drop_authority(&self, name: &str) -> Result<(), SQLError>;
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
    pub deletion: &'a dyn crate::schema::deletion::CatalogRemovalInputs,
    pub catalog: &'a dyn RelationDropCatalog,
    pub tables: crate::schema::table_removal::context::TableRemovalContext<'a>,
    pub privileges: &'a dyn RelationRemovalPrivileges,
    pub views: &'a dyn RelationRemovalViews,
    pub sequences: &'a dyn crate::schema::sequences::removal::SequenceRemovalInputs,
    pub identities: &'a dyn RelationLockCatalog,
    pub locks: &'a dyn RelationDefinitionSession,
    pub transactions: &'a dyn RelationRemovalTransactions,
    pub notices: &'a parking_lot::Mutex<Vec<uqa_sql::SQLNotice>>,
    pub indexes: IndexRemovalContext<'a>,
}
