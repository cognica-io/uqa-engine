//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable index-expression binding in the indexed table's declared row type.

use super::{generated::bind_schema_column_references, SchemaExpressionCatalog};
use crate::plan::ExpressionPlan;
use crate::RowSchema;
use crate::{ast::Expr, binding::context::BindingContext, ColumnType, SQLError};

pub fn prepare_index_expression(
    engine: &dyn SchemaExpressionCatalog,
    binding: &BindingContext<'_>,
    table: &str,
    expression: &mut Expr,
) -> Result<ColumnType, SQLError> {
    let ty = bind_index_expression(engine, binding, table, expression, false)?;
    validate_index_expression_immutability(engine, table, expression, false)?;
    if let Some(ty) = ty {
        return Ok(ty);
    }
    *expression = Expr::Cast {
        implicit: true,
        expr: Box::new(expression.clone()),
        ty: "text".into(),
    };
    Ok(ColumnType::Text)
}

pub fn prepare_index_predicate(
    engine: &dyn SchemaExpressionCatalog,
    binding: &BindingContext<'_>,
    table: &str,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    bind_index_predicate(engine, binding, table, expression)?;
    validate_index_expression_immutability(engine, table, expression, true)
}

pub(super) fn bind_index_predicate(
    engine: &dyn SchemaExpressionCatalog,
    binding: &BindingContext<'_>,
    table: &str,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    match bind_index_expression(engine, binding, table, expression, true)? {
        Some(ref ty) if crate::type_resolution::base_type(ty) == &ColumnType::Boolean => Ok(()),
        None => {
            if let Expr::Literal(value) = expression {
                *value = crate::expr::cast_value(value, "boolean")?;
            }
            Ok(())
        }
        Some(_) => Err(SQLError::TypeMismatch(
            "argument of WHERE must be type boolean".into(),
        )),
    }
}

pub(super) fn bind_index_expression(
    engine: &dyn SchemaExpressionCatalog,
    binding: &BindingContext<'_>,
    table: &str,
    expression: &mut Expr,
    predicate: bool,
) -> Result<Option<ColumnType>, SQLError> {
    let context = if predicate {
        "index predicate"
    } else {
        "index expression"
    };
    let mut plan = ExpressionPlan::lower(expression.clone());
    if !plan.subqueries.is_empty() {
        return Err(index_error(
            "0A000",
            format!("cannot use subquery in {context}"),
        ));
    }
    if crate::semantics::aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(index_error(
            "42803",
            format!("aggregate functions are not allowed in {context}s"),
        ));
    }
    if crate::semantics::windows::expr_has_window(&plan.scalar) {
        return Err(index_error(
            "42P20",
            format!("window functions are not allowed in {context}s"),
        ));
    }
    let columns = engine
        .schema_expression_columns(table)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    *expression = super::expressions::bind_relation_row_references(
        expression,
        table,
        &relation.name,
        &relation.name,
        &columns,
    )?;
    bind_schema_column_references(expression, &relation.name);
    bind_schema_column_references(expression, table);
    plan.scalar = ExpressionPlan::lower(expression.clone()).scalar;
    let row_type = crate::FunctionTypeResolver::resolve_type_name(engine, table)?;
    let schema = row_schema(table, &columns, row_type.as_ref())?;
    if crate::semantics::sets::validation::expression_may_return_set(
        engine,
        engine,
        &plan.scalar,
        &schema,
        &[],
    )? {
        return Err(index_error(
            "0A000",
            format!("set-returning functions are not allowed in {context}s"),
        ));
    }
    let lowered = plan.clone();
    let ty = crate::binding::bind_expression_plan_routines_for_storage(
        engine,
        &mut plan,
        &[],
        binding,
        &schema,
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&lowered, &plan)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)?;
    Ok(ty)
}

pub(super) fn validate_index_expression_immutability(
    engine: &dyn SchemaExpressionCatalog,
    table: &str,
    expression: &mut Expr,
    predicate: bool,
) -> Result<(), SQLError> {
    let columns = engine
        .schema_expression_columns(table)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let row_type = crate::FunctionTypeResolver::resolve_type_name(engine, table)?;
    let schema = row_schema(table, &columns, row_type.as_ref())?;
    let planned = engine.plan_schema_expression(expression, &schema)?;
    if !planned.immutable {
        let context = if predicate { "predicate" } else { "expression" };
        return Err(index_error(
            "42P17",
            format!("functions in index {context} must be marked IMMUTABLE"),
        ));
    }
    *expression = planned.expression;
    Ok(())
}

fn index_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

/// The indexed relation supplies both its column namespace and its actual named row type.
pub fn row_schema(
    table: &str,
    columns: &[crate::ast::ColumnDef],
    row_type: Option<&ColumnType>,
) -> Result<RowSchema, SQLError> {
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    row_schema_with_control(
        Some(&relation.name),
        Some(columns),
        row_type,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

/// Restore analysis keeps the original admitted row layout without resolving a new declaration.
pub fn row_schema_with_control(
    qualifier: Option<&str>,
    columns: Option<&[crate::ast::ColumnDef]>,
    row_type: Option<&ColumnType>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let mut schema = RowSchema::with_column_metadata_with_control(
        qualifier,
        columns
            .into_iter()
            .flatten()
            .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
        control,
    )?;
    if columns.is_none() {
        schema = RowSchema::with_open_columns_with_control(&schema, qualifier, control)?;
    }
    if let Some((qualifier, row_type)) = qualifier.zip(row_type) {
        schema = schema.with_relation_row_type_with_control(qualifier, row_type, control)?;
    }
    Ok(schema)
}

pub mod keys;

pub mod names;

pub mod unique;

pub mod options;
pub mod system_columns;
pub mod vectors;

pub mod removal;
pub mod routines;
