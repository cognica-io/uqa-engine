//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply fresh foreign catalog inputs after entering the existing implicit transaction.
use super::ForeignCreationContext;
use uqa_sql::{
    ast::{ColumnDef, DeferredCreateForeignTable, TableCheck},
    SQLError,
};
pub type ForeignCatalogWrite<'a> =
    Box<dyn FnOnce(&ForeignCreationContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait ForeignCreationTransactions {
    fn with_foreign_catalog_write(&self, write: ForeignCatalogWrite<'_>) -> Result<(), SQLError>;
}
pub fn register_foreign_wrapper_statement(
    transactions: &dyn ForeignCreationTransactions,
    statement: uqa_sql::ast::CreateForeignWrapper,
) -> Result<(), SQLError> {
    transactions.with_foreign_catalog_write(Box::new(move |context| {
        context.register_foreign_wrapper_statement(&statement)
    }))
}
pub fn register_foreign_server_statement(
    transactions: &dyn ForeignCreationTransactions,
    statement: uqa_sql::ast::CreateForeignServer,
) -> Result<(), SQLError> {
    transactions.with_foreign_catalog_write(Box::new(move |context| {
        context.register_foreign_server_statement(&statement)
    }))
}
pub fn register_foreign_table_with_checks(
    transactions: &dyn ForeignCreationTransactions,
    name: String,
    server_name: String,
    columns: Vec<ColumnDef>,
    checks: Vec<TableCheck>,
    options: Vec<(String, String)>,
    if_not_exists: bool,
) -> Result<(), SQLError> {
    transactions.with_foreign_catalog_write(Box::new(move |context| {
        context.register_foreign_table_inner(
            &name,
            server_name,
            columns,
            checks,
            options,
            if_not_exists,
        )
    }))
}
pub fn register_deferred_foreign_table(
    transactions: &dyn ForeignCreationTransactions,
    deferred: DeferredCreateForeignTable,
) -> Result<(), SQLError> {
    transactions.with_foreign_catalog_write(Box::new(move |context| {
        context.register_deferred_foreign_table(deferred)
    }))
}

/// Publish an already analyzed SQL definition while retaining its written NOT NULL declarations.
pub fn register_foreign_table_statement(
    transactions: &dyn ForeignCreationTransactions,
    statement: uqa_sql::ast::CreateForeignTable,
) -> Result<(), SQLError> {
    transactions.with_foreign_catalog_write(Box::new(move |context| {
        context.register_foreign_table_statement(statement)
    }))
}
