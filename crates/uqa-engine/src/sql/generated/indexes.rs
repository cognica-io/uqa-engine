//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable index-expression binding in the indexed table's declared row type.

use super::{bind_generation_column_references, typing, ColumnType, Engine, Expr, SQLError};
use uqa_execution::RowSchema;
use uqa_planner::ExpressionPlan;

pub(in crate::sql) fn prepare_index_expression(
    engine: &Engine,
    table: &str,
    expression: &mut Expr,
) -> Result<ColumnType, SQLError> {
    let ty = bind_immutable_index_expression(engine, table, expression, false)?;
    if let Some(ty) = ty {
        return Ok(ty);
    }
    *expression = Expr::Cast {
        expr: Box::new(expression.clone()),
        ty: "text".into(),
    };
    Ok(ColumnType::Text)
}

pub(in crate::sql) fn prepare_index_predicate(
    engine: &Engine,
    table: &str,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    match bind_immutable_index_expression(engine, table, expression, true)? {
        Some(ColumnType::Boolean) => Ok(()),
        None => {
            if let Expr::Literal(value) = expression {
                *value = uqa_sql::expr::cast_value(value, "boolean")?;
            }
            Ok(())
        }
        Some(_) => Err(SQLError::TypeMismatch(
            "argument of WHERE must be type boolean".into(),
        )),
    }
}

fn bind_immutable_index_expression(
    engine: &Engine,
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
    if crate::sql::aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(index_error(
            "42803",
            format!("aggregate functions are not allowed in {context}s"),
        ));
    }
    if crate::sql::window::expr_has_window(&plan.scalar) {
        return Err(index_error(
            "42P20",
            format!("window functions are not allowed in {context}s"),
        ));
    }
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    bind_generation_column_references(expression, &relation.name);
    bind_generation_column_references(expression, table);
    plan.scalar = ExpressionPlan::lower(expression.clone()).scalar;
    let schema = RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    if crate::sql::select::expression_may_return_set(engine, engine, &plan.scalar, &schema, &[])? {
        return Err(index_error(
            "0A000",
            format!("set-returning functions are not allowed in {context}s"),
        ));
    }
    typing::infer_generation_expression(engine, &columns, expression)?;
    let ty =
        crate::sql::bind_catalog_expression_routines_with_outer(engine, &mut plan, &[], &schema)?;
    let references = crate::sql::collect_expression_routine_references(&plan)?;
    crate::engine_events::bind_stored_expression_routines(expression, &references)?;
    Ok(ty)
}

fn index_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
