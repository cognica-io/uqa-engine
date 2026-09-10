//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities required by query tuple locking, separated by state ownership.

use super::{
    CteScope, LockStrength, PhysicalOperator, QueryBlockPlan, QueryPlan, RelationPersistence,
    RowLockRetryCache, SQLError, SQLParam,
};
use crate::row_locks::{
    retry_cache::CommittedRowSource, session::RowLockSession, RelationLockMode, RowLockAcquisition,
};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{CancellationToken, DocId};
use uqa_sql::{
    ast::{ColumnDef, TableKeyConstraint},
    routines::RoutineResolution,
};

pub trait LockingCatalog: RoutineResolution {
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError>;
    fn resolve_relation(
        &self,
        name: &str,
        bound: bool,
    ) -> Result<Option<(String, &'static str)>, SQLError>;
    fn view_plan(&self, name: &str) -> Result<Option<QueryPlan>, SQLError>;
    fn table_persistence(&self, name: &str) -> Result<Option<RelationPersistence>, String>;
    fn referenceable_keys(&self, table: &str) -> Result<Vec<TableKeyConstraint>, String>;
    fn table_columns(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
}

pub trait QueryRowLockSession: RowLockSession + Sync {
    fn lock_relation(&self, table: &str, mode: RelationLockMode) -> Result<(), SQLError>;
    fn rollback_row_lock_acquisition(&self, acquisition: RowLockAcquisition);
    fn row_changed_in_open_transaction(&self, table: &str, doc_id: DocId)
        -> Result<bool, SQLError>;
    fn row_lock_change_requires_recheck(&self) -> Result<bool, SQLError>;
    fn current_transaction_is_read_only(&self) -> bool;
    fn statement_row_lock_cache(&self) -> Result<Arc<RowLockRetryCache>, SQLError>;
}

pub trait RowLockReadSource: CommittedRowSource + Sync {
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
    fn get_document_for_mutation(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<Document>, SQLError>;
}

pub trait RowLockScopeSource<S: Clone>: Sync {
    fn current_routine_scope(&self) -> CteScope<S>;
    fn transition_relation_names(&self) -> BTreeSet<String>;
}

pub trait RowRecheckBuilder<S: Clone>: Sync {
    fn build_recheck<'a>(
        &'a self,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],
        ctes: &mut CteScope<S>,
        ordered: bool,
        projections: &[super::super::PhysicalProjection],
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError>;
}

#[derive(Clone)]
pub struct RowLockContext<'a, S: Clone> {
    pub catalog: &'a dyn LockingCatalog,
    pub session: &'a dyn QueryRowLockSession,
    pub rows: &'a dyn RowLockReadSource,
    pub scopes: &'a dyn RowLockScopeSource<S>,
    pub recheck: &'a dyn RowRecheckBuilder<S>,
    pub cancellation: &'a CancellationToken,
}

impl<S: Clone> Copy for RowLockContext<'_, S> {}

pub fn update_lock_strength(
    catalog: &dyn LockingCatalog,
    table: &str,
    columns: &[String],
) -> LockStrength {
    let Ok(keys) = catalog.referenceable_keys(table) else {
        return LockStrength::ForUpdate;
    };
    let Ok(Some(definitions)) = catalog.table_columns(table) else {
        return LockStrength::ForUpdate;
    };
    uqa_sql::semantics::locking::update_lock_strength(&keys, &definitions, columns)
}

use uqa_storage::document_store::Document;
