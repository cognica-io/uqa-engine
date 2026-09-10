//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind declared column metadata and publish rewritten storage values.

use super::{ColumnType, Engine, SQLError, Value};
pub(crate) use uqa_sql::assignment::conversion::convert_value_to_column_type_with_context as convert_value_to_column_type_with_engine;
pub(crate) use uqa_sql::assignment::conversion::*;
use uqa_sql::ast::Expr;

/// Coerce a write value to fit the column's declared type.
pub(in crate::sql) fn coerce_to_column_type(
    engine: &Engine,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    uqa_sql::assignment::columns::coerce_to_column_type(engine, engine, table, column, value)
}

pub(super) fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<(), SQLError> {
    uqa_execution::schema::columns::rewrite_column_values_to_type(
        &uqa_execution::schema::columns::ColumnRewriteContext {
            columns: engine,
            reads: engine,
            types: engine,
            expressions: engine,
            writes: engine,
        },
        table,
        column,
        source_ty,
        target_ty,
        using,
    )
}
