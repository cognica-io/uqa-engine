//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred execution inputs and session-owned cache, transaction and lock state.

use crate::{
    row_locks::retry_cache::RowLockRetryCache,
    statement::{
        context::{StatementEffects, StatementExecutionInputs, StatementRuntime},
        transactions::StatementTransactions,
    },
};
use std::sync::Arc;
use uqa_sql::{
    ast::TransactionStmt,
    plan::{AggregateClassifier, ExecutablePlanOptimizer, UnifiedPlan},
    SQLError, Statement,
};

/// Owned cache contents after the session has validated its catalog epochs.
pub struct CachedStatement {
    pub statement: Arc<Statement>,
    pub logical_plan: Arc<UnifiedPlan>,
    pub optimized_plan: Option<Arc<UnifiedPlan>>,
}

pub trait StatementCache {
    fn cached_sql_statement(&self, sql: &str) -> Option<CachedStatement>;
    fn cached_optimized_sql_plan(&self, sql: &str) -> Option<Arc<UnifiedPlan>>;
    fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<Statement>,
        logical_plan: Arc<UnifiedPlan>,
    );
    fn cache_optimized_sql_plan(&self, sql: &str, optimized_plan: Arc<UnifiedPlan>);
}

/// Transaction state changes requested at the native scheduler's boundaries.
pub trait BatchTransactions: StatementTransactions {
    fn begin_simple_query_transaction(&self) -> Result<(), SQLError>;
    fn promote_simple_query_transaction(&self) -> Result<(), SQLError>;
    fn run_transaction_statement(&self, statement: TransactionStmt) -> Result<(), SQLError>;
    fn ensure_transaction_usable(&self) -> Result<(), SQLError>;
    fn prepare_explicit_statement_snapshot(&self, sets_snapshot: bool) -> Result<(), SQLError>;
    fn prepare_explicit_transaction_writer(&self) -> Result<bool, SQLError>;
    fn begin_implicit_statement_transaction(&self, read_only: bool) -> Result<(), SQLError>;
}

/// Retain the session's row-lock frame until the current loop iteration exits.
pub trait RowLockStatementGuard {}

pub trait BatchRowLocks {
    fn begin_row_lock_statement(&self) -> Box<dyn RowLockStatementGuard + '_>;
    fn statement_row_lock_cache(&self) -> Result<Arc<RowLockRetryCache>, SQLError>;
}

pub struct BatchExecutionContext<'a, S: Clone + 'static> {
    pub runtime: StatementRuntime<'a>,
    pub persistent_backend: bool,
    pub statements: &'a dyn StatementExecutionInputs<S>,
    pub cache: &'a dyn StatementCache,
    pub aggregates: &'a dyn AggregateClassifier,
    pub effects: &'a dyn StatementEffects,
    pub planning: &'a dyn ExecutablePlanOptimizer,
    pub transactions: &'a dyn BatchTransactions,
    pub row_locks: &'a dyn BatchRowLocks,
}
