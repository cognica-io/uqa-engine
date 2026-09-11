//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind statement validation and failure cleanup to the live transaction stack.

use crate::Engine;
use uqa_execution::statement::transactions::StatementTransactions;
use uqa_sql::{ast::TransactionStmt, SQLError};

impl StatementTransactions for Engine {
    fn transaction_depth(&self) -> usize {
        Engine::transaction_depth(self)
    }
    fn current_transaction_is_read_only(&self) -> bool {
        Engine::current_transaction_is_read_only(self)
    }
    fn mark_transaction_snapshot_set(&self) {
        Engine::mark_transaction_snapshot_set(self);
    }
    fn abort_after_error(&self, error: SQLError) -> SQLError {
        self.abort_sql_transaction_after_error(error)
    }
    fn rollback(&self) -> Result<(), SQLError> {
        self.run_transaction_statement(TransactionStmt::Rollback)
    }
}
