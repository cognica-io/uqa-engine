//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical index expression evaluation to the current command.
use crate::Engine;
use uqa_core::Value;
use uqa_sql::{ast::Expr, SQLError};
use uqa_storage::document_store::Document;

pub(crate) fn index_predicate_accepts(
    engine: &Engine,
    table: &str,
    predicate: Option<&Expr>,
    document: &Document,
) -> Result<bool, SQLError> {
    uqa_execution::mutation::constraints::index_keys::index_predicate_accepts(
        engine.constraint_execution_context(),
        table,
        predicate,
        document,
    )
}

pub(crate) fn index_key_values(
    engine: &Engine,
    table: &str,
    keys: &[uqa_sql::ast::IndexKey],
    document: &Document,
) -> Result<Vec<Value>, SQLError> {
    uqa_execution::mutation::constraints::index_keys::index_key_values(
        engine.constraint_execution_context(),
        table,
        keys,
        document,
    )
}
