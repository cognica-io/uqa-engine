//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply the statement's immutable catalog context for SQL declaration validation.

use crate::Engine;
use uqa_sql::{
    ast::{ColumnType, Expr},
    SQLError,
};

pub(crate) fn validate_default_expression(
    engine: &Engine,
    expression: &mut Expr,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::defaults::validate_default_expression(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: engine,
            binding: &binding,
        },
        expression,
        target,
    )
}

pub(crate) fn bind_stored_schema_expression_routines(
    engine: &Engine,
    expression: &mut Expr,
    typed_expression: Expr,
) -> Result<bool, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::defaults::bind_stored_schema_expression_routines(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: engine,
            binding: &binding,
        },
        expression,
        typed_expression,
    )
}
