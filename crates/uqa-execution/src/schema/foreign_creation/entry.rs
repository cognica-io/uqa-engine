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
pub type ForeignServerWrite<'a> =
    Box<dyn FnOnce(&ForeignCreationContext<'_>) -> Result<(), String> + 'a>;
pub type ForeignTableWrite<'a> =
    Box<dyn FnOnce(&ForeignCreationContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait ForeignCreationTransactions {
    fn with_foreign_server_write(&self, write: ForeignServerWrite<'_>) -> Result<(), String>;
    fn with_foreign_table_write(&self, write: ForeignTableWrite<'_>) -> Result<(), SQLError>;
}
pub fn register_foreign_server(
    transactions: &dyn ForeignCreationTransactions,
    name: String,
    fdw_type: String,
    options: Vec<(String, String)>,
    if_not_exists: bool,
) -> Result<(), String> {
    transactions.with_foreign_server_write(Box::new(move |context| {
        context.register_foreign_server_inner(name, &fdw_type, options, if_not_exists)
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
    transactions.with_foreign_table_write(Box::new(move |context| {
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
    transactions.with_foreign_table_write(Box::new(move |context| {
        context.register_deferred_foreign_table(deferred)
    }))
}
