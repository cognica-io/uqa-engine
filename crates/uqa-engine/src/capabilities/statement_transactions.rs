//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind statement validation and failure cleanup to the live transaction stack.

use crate::Engine;
use uqa_execution::statement::transactions::StatementTransactions;
use uqa_sql::{ast::TransactionStmt, SQLError};

impl uqa_execution::statement::batch::context::BatchTransactions for Engine {
    fn begin_simple_query_transaction(&self) -> Result<(), SQLError> {
        Engine::begin_simple_query_transaction(self)
    }
    fn promote_simple_query_transaction(&self) -> Result<(), SQLError> {
        Engine::promote_simple_query_transaction(self)
    }
    fn run_transaction_statement(&self, statement: TransactionStmt) -> Result<(), SQLError> {
        Engine::run_transaction_statement(self, statement)
    }
    fn ensure_transaction_usable(&self) -> Result<(), SQLError> {
        Engine::ensure_transaction_usable(self)
    }
    fn prepare_explicit_statement_snapshot(&self, sets_snapshot: bool) -> Result<(), SQLError> {
        Engine::prepare_explicit_statement_snapshot(self, sets_snapshot)
    }
    fn prepare_explicit_transaction_writer(&self) -> Result<bool, SQLError> {
        Engine::prepare_explicit_transaction_writer(self)
    }
    fn begin_implicit_statement_transaction(&self, read_only: bool) -> Result<(), SQLError> {
        Engine::begin_implicit_statement_transaction(self, read_only)
    }
}

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
