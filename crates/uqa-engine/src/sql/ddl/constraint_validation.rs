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

pub(crate) fn bind_stored_check_expression_routines(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<bool, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::constraints::bind_stored_check_expression_routines(
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

use uqa_sql::ast::TableKeyConstraint;
pub(super) use uqa_sql::schema::constraints::validate_foreign_key_definition;

pub(super) fn resolve_foreign_key_parent(
    engine: &Engine,
    reference: &str,
) -> Result<(String, Vec<ColumnDef>, Vec<TableKeyConstraint>), SQLError> {
    uqa_sql::schema::foreign_keys::resolve_foreign_key_parent(
        &engine.foreign_key_definition_context(),
        reference,
    )
}
