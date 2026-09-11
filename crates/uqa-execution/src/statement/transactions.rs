//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only validation, snapshot marking, and statement failure cleanup.

use uqa_sql::{
    plan::UnifiedPlan,
    semantics::effects::{
        read_only::{forbidden_command, plan_sets_transaction_snapshot, read_only_error},
        QueryEffectContext,
    },
    SQLError,
};

/// Session transaction observations and the existing rollback/abort boundary.
pub trait StatementTransactions {
    fn transaction_depth(&self) -> usize;
    fn current_transaction_is_read_only(&self) -> bool;
    fn mark_transaction_snapshot_set(&self);
    fn abort_after_error(&self, error: SQLError) -> SQLError;
    fn rollback(&self) -> Result<(), SQLError>;
}

pub fn validate_transaction_plan(
    transactions: &dyn StatementTransactions,
    effects: &QueryEffectContext<'_>,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    if transactions.current_transaction_is_read_only() {
        if let Some(command) = forbidden_command(effects, plan)? {
            return Err(read_only_error(command));
        }
    }
    if plan_sets_transaction_snapshot(plan) {
        transactions.mark_transaction_snapshot_set();
    }
    Ok(())
}

pub fn abort_explicit_statement_error(
    transactions: &dyn StatementTransactions,
    error: SQLError,
) -> SQLError {
    if transactions.transaction_depth() == 0 {
        error
    } else {
        transactions.abort_after_error(error)
    }
}

pub fn rollback_implicit_statement(
    transactions: &dyn StatementTransactions,
    action: &str,
) -> Result<(), SQLError> {
    transactions.rollback().map_err(|rollback_error| {
        SQLError::Internal(format!(
            "{action}: autocommit rollback failed: {rollback_error}"
        ))
    })
}

pub fn rollback_after_statement_error<T>(
    transactions: &dyn StatementTransactions,
    statement_error: SQLError,
) -> Result<T, SQLError> {
    if transactions.transaction_depth() == 0 {
        return Err(statement_error);
    }
    match transactions.rollback() {
        Ok(()) => Err(statement_error),
        Err(rollback_error) => Err(SQLError::Internal(format!(
            "statement failed: {statement_error}; autocommit rollback also failed: {rollback_error}"
        ))),
    }
}

#[cfg(test)]
mod tests;
