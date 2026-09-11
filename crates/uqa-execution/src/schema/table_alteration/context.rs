//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inputs for ALTER TABLE execution after relation resolution and transaction entry.
use crate::schema::{
    columns::{
        addition::ColumnAdditionContext, alteration::ColumnAlterContext,
        removal::ColumnRemovalContext,
    },
    constraints::ConstraintAlterContext,
    hierarchy::HierarchyContext,
};
use uqa_sql::{ast::EventEnableMode, SQLError};
use uqa_storage::StorageBackendResult;

pub trait TableLifecycle {
    fn has_table(&self, table: &str) -> StorageBackendResult<bool>;
    fn rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool>;
    fn rename_column(&self, table: &str, from: &str, to: &str) -> StorageBackendResult<bool>;
}
pub trait TableEventLifecycle {
    fn rename_trigger(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError>;
    fn rename_trigger_constraint(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError>;
    fn rename_rule(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError>;
    fn set_trigger_enable_mode(
        &self,
        table: &str,
        name: Option<&str>,
        mode: EventEnableMode,
    ) -> Result<(), SQLError>;
    fn set_rule_enable_mode(
        &self,
        table: &str,
        name: &str,
        mode: EventEnableMode,
    ) -> Result<(), SQLError>;
}
pub struct TableAlterContext<'a, S: Clone + 'static> {
    pub ownership: crate::catalog::security::table_ownership::TableOwnershipContext<'a>,
    pub hierarchy: HierarchyContext<'a>,
    pub constraints: ConstraintAlterContext<'a>,
    pub addition: ColumnAdditionContext<'a, S>,
    pub columns: ColumnAlterContext<'a, S>,
    pub removal: ColumnRemovalContext<'a>,
    pub lifecycle: &'a dyn TableLifecycle,
    pub events: &'a dyn TableEventLifecycle,
}
