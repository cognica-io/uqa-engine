//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable index-expression binding in the indexed table's declared row type.

use super::{
    generated::{bind_schema_column_references, typing},
    SchemaExpressionCatalog,
};
use crate::plan::ExpressionPlan;
use crate::RowSchema;
use crate::{ast::Expr, binding::context::BindingContext, ColumnType, SQLError};

pub fn prepare_index_expression(
    engine: &dyn SchemaExpressionCatalog,
    binding: &BindingContext<'_>,
    table: &str,
    expression: &mut Expr,
) -> Result<ColumnType, SQLError> {
    let ty = bind_immutable_index_expression(engine, binding, table, expression, false)?;
    if let Some(ty) = ty {
        return Ok(ty);
    }
    *expression = Expr::Cast {
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
    match bind_immutable_index_expression(engine, binding, table, expression, true)? {
        Some(ColumnType::Boolean) => Ok(()),
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

fn bind_immutable_index_expression(
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
    bind_schema_column_references(expression, &relation.name);
    bind_schema_column_references(expression, table);
    plan.scalar = ExpressionPlan::lower(expression.clone()).scalar;
    let schema = RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
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
    typing::infer_generation_expression(engine, &columns, expression)?;
    let ty = crate::binding::bind_expression_plan_routines_for_storage(
        engine,
        &mut plan,
        &[],
        binding,
        &schema,
    )?;
    let references = crate::binding::stored_routines::collect_expression_routine_references(&plan)?;
    crate::catalog::stored_ast::bind_stored_expression_routines(expression, &references)?;
    Ok(ty)
}

fn index_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
