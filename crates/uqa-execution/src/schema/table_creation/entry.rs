//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enter each CREATE TABLE command in its own transaction frame with fresh catalog inputs.
use super::CreateTableContext;
use uqa_sql::{
    ast::{CreateTable, DeferredCreateTable},
    SQLError, SQLResult,
};

pub type TableCreationWrite<'a> =
    Box<dyn FnOnce(&CreateTableContext<'_>) -> Result<SQLResult, SQLError> + 'a>;

pub trait TableCreationTransactions {
    fn with_new_table_transaction(
        &self,
        write: TableCreationWrite<'_>,
    ) -> Result<SQLResult, SQLError>;
}

pub fn run_create_table(
    transactions: &dyn TableCreationTransactions,
    statement: CreateTable,
) -> Result<SQLResult, SQLError> {
    transactions.with_new_table_transaction(Box::new(move |context| {
        super::run_create_table(context, statement)
    }))
}

pub fn run_create_table_if_not_exists(
    transactions: &dyn TableCreationTransactions,
    statement: DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    transactions.with_new_table_transaction(Box::new(move |context| {
        super::run_create_table_if_not_exists(context, statement)
    }))
}
