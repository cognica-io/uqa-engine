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
    pub row_changes: &'a dyn crate::schema::columns::rows::changes::RewriteRowChanges,
    pub binding: super::binding::TableAlterBindingContext<'a>,
    pub ownership: crate::catalog::security::table_ownership::TableOwnershipContext<'a>,
    pub hierarchy: HierarchyContext<'a>,
    pub constraints: ConstraintAlterContext<'a>,
    pub addition: ColumnAdditionContext<'a, S>,
    pub columns: ColumnAlterContext<'a, S>,
    /// Planner-owned simplification after SQL has resolved the transform and its assignment type.
    pub plan_type_transform: fn(&mut uqa_sql::ScalarExpr) -> Result<(), SQLError>,
    pub removal: ColumnRemovalContext<'a>,
    pub identities: super::identity::IdentityAlterContext<'a>,
    pub lifecycle: &'a dyn TableLifecycle,
    pub events: &'a dyn TableEventLifecycle,
}
