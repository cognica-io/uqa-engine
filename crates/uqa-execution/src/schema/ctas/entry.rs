//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse an active transaction for CREATE TABLE AS and capture its analysis scope at execution.
use super::{CreateTableAsContext, CreateTableAsExecution};
use uqa_sql::{SQLError, SQLResult};

pub type TableAsWrite<'a, S> =
    Box<dyn FnOnce(&CreateTableAsContext<'_, S>) -> Result<SQLResult, SQLError> + 'a>;

pub trait TableAsTransactions<S: Clone> {
    fn transaction_is_active(&self) -> bool;
    fn with_new_transaction(&self, write: TableAsWrite<'_, S>) -> Result<SQLResult, SQLError>;
    fn with_current_scope(&self, write: TableAsWrite<'_, S>) -> Result<SQLResult, SQLError>;
}

pub fn run_create_table_as<S: Clone>(
    transactions: &dyn TableAsTransactions<S>,
    execution: CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    let active = transactions.transaction_is_active();
    let write: TableAsWrite<'_, S> =
        Box::new(move |context| super::run_create_table_as(context, &execution));
    if active {
        transactions.with_current_scope(write)
    } else {
        transactions.with_new_transaction(write)
    }
}
