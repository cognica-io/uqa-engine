//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provide the current catalog binding scope for index declarations.

use crate::Engine;
use uqa_sql::{
    ast::{ColumnType, Expr},
    SQLError,
};

pub(in crate::sql) fn prepare_index_expression(
    engine: &Engine,
    table: &str,
    expression: &mut Expr,
) -> Result<ColumnType, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::indexes::prepare_index_expression(engine, &binding, table, expression)
}

pub(in crate::sql) fn prepare_index_predicate(
    engine: &Engine,
    table: &str,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::indexes::prepare_index_predicate(engine, &binding, table, expression)
}
