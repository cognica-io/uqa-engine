//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply the statement's immutable catalog context for SQL declaration validation.

use crate::Engine;
use uqa_sql::{
    ast::{ColumnDef, Expr},
    SQLError,
};

pub(crate) fn validate_check_expression(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::constraints::validate_check_expression(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: engine,
            binding: &binding,
        },
        table,
        qualifier,
        columns,
        expression,
    )
}
