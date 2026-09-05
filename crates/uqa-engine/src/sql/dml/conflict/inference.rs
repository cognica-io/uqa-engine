//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-index arbitration and strong predicate implication.

use crate::engine_catalog_indexes::EnforcedKey;
use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_execution::{RowSchema, ScalarExpr as Expr};
use uqa_planner::{ConflictPlan, ExpressionPlan, InsertPlan};
use uqa_sql::ast::BinaryOp;
use uqa_sql::{SQLError, SQLParam};

/// Analyze the inference clause in the INSERT target's scope before uniqueness arbitration, including commands that produce no input rows.
pub(in crate::sql) fn prepare_inference_predicate<'a>(
    engine: &Engine,
    statement: &'a InsertPlan,
    params: &[SQLParam],
) -> Result<std::borrow::Cow<'a, InsertPlan>, SQLError> {
    if statement
        .on_conflict
        .as_ref()
        .is_none_or(|conflict| conflict.predicate.is_none() && conflict.expressions.is_empty())
    {
        return Ok(std::borrow::Cow::Borrowed(statement));
    }
    let columns = engine
        .try_describe_table(&statement.table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(statement.table.clone()))?;
    let schema = RowSchema::with_qualified_types(
        &statement.target_qualifier,
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let mut statement = statement.clone();
    if let Some(conflict) = &mut statement.on_conflict {
        for expression in conflict
            .expressions
            .iter_mut()
            .chain(conflict.predicate.iter_mut().map(Box::as_mut))
        {
            prepare_inference_expression(
                engine,
                expression,
                &statement.target_qualifier,
                &schema,
                &columns,
                params,
            )?;
        }
        // A rewritten view expression can resolve to a simple base-table attribute.
        let expressions = std::mem::take(&mut conflict.expressions);
        for expression in expressions {
            if let Expr::Column(column) = expression {
                conflict.conflict_columns.push(column);
            } else {
                conflict.expressions.push(expression);
            }
        }
    }
    Ok(std::borrow::Cow::Owned(statement))
}

fn prepare_inference_expression(
    engine: &Engine,
    expression: &mut Expr,
    qualifier: &str,
    schema: &RowSchema,
    columns: &[uqa_sql::ast::ColumnDef],
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let mut has_subquery = false;
    expression.visit(&mut |part| {
        has_subquery |= matches!(
            part,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        );
    });
    if has_subquery {
        return Err(predicate_error(
            "0A000",
            "cannot use subquery in index inference",
        ));
    }
    if crate::sql::aggregates::contains_aggregate(engine, expression) {
        return Err(predicate_error(
            "42803",
            "aggregate functions are not allowed in index inference",
        ));
    }
    if crate::sql::window::expr_has_window(expression) {
        return Err(predicate_error(
            "42P20",
            "window functions are not allowed in index inference",
        ));
    }
    let mut plan = ExpressionPlan {
        scalar: expression.clone(),
        subqueries: Vec::new(),
    };
    crate::sql::bind_catalog_expression_routines_with_outer(engine, &mut plan, params, schema)?;
    uqa_planner::rewrite_scalar_expression(&mut plan.scalar, &mut |expression| {
        if let Expr::QualifiedColumn {
            qualifier: source,
            column,
        } = expression
        {
            if source == qualifier {
                *expression = Expr::Column(column.clone());
            }
        }
    });
    uqa_planner::rewrite_scalar_expression(&mut plan.scalar, &mut |expression| {
        if let Expr::Cast { expr, ty } = expression {
            if let Expr::Column(name) = expr.as_ref() {
                if columns.iter().any(|column| {
                    column.name == *name
                        && uqa_sql::ast::ColumnType::from_sql_name(ty).ok().as_ref()
                            == Some(&column.ty)
                }) {
                    *expression = Expr::Column(name.clone());
                }
            }
        }
    });
    *expression = plan.scalar;
    Ok(())
}

fn predicate_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

pub(super) fn conflict_key_indices(
    engine: &Engine,
    table: &str,
    keys: &[EnforcedKey],
    conflict: &ConflictPlan,
) -> Result<Vec<usize>, SQLError> {
    if let Some(name) = &conflict.constraint {
        return constraint_target_index(engine, table, keys, name);
    }
    if conflict.conflict_columns.is_empty() && conflict.expressions.is_empty() {
        return Ok((0..keys.len()).collect());
    }
    validate_conflict_columns(engine, table, &conflict.conflict_columns)?;
    let target = conflict
        .conflict_columns
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let indexes = keys
        .iter()
        .enumerate()
        .filter_map(|(index, key)| {
            (key.columns
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == target
                && {
                    let expressions = key
                        .keys
                        .iter()
                        .filter_map(|key| match key {
                            uqa_sql::ast::IndexKey::Expression(expr) => {
                                Some(ExpressionPlan::lower((**expr).clone()).scalar)
                            }
                            uqa_sql::ast::IndexKey::Column(_) => None,
                        })
                        .collect::<Vec<_>>();
                    expressions
                        .iter()
                        .all(|expr| conflict.expressions.contains(expr))
                        && conflict
                            .expressions
                            .iter()
                            .all(|expr| expressions.contains(expr))
                }
                && key.predicate.as_deref().is_none_or(|required| {
                    conflict.predicate.as_deref().is_some_and(|given| {
                        implies(given, &ExpressionPlan::lower(required.clone()).scalar)
                    })
                }))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if indexes.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message:
                "there is no unique or exclusion constraint matching the ON CONFLICT specification"
                    .into(),
        });
    }
    Ok(indexes)
}

/// Prove that every row for which `given` is true also makes `required` true. SQL NULL is never treated as false in a proof that would admit an invalid arbiter.
fn implies(given: &Expr, required: &Expr) -> bool {
    if given == required || matches!(required, Expr::Literal(Value::Bool(true))) {
        return true;
    }
    if matches!(given, Expr::Literal(Value::Bool(false) | Value::Null)) {
        return true;
    }
    if let Expr::And(parts) = required {
        return parts.iter().all(|part| implies(given, part));
    }
    if let Expr::Or(parts) = given {
        return parts.iter().all(|part| implies(part, required));
    }
    if let Expr::And(parts) = given {
        if parts.iter().any(|part| implies(part, required)) {
            return true;
        }
    }
    if let Expr::Or(parts) = required {
        return parts.iter().any(|part| implies(given, part));
    }
    if let Some((left, given_op, given_value)) = comparison(given) {
        if let Some((right, required_op, required_value)) = comparison(required) {
            return left == right
                && comparison_implies(given_op, given_value, required_op, required_value);
        }
        if let Expr::IsNull {
            expr,
            negated: true,
        } = required
        {
            return left == expr.as_ref() && !matches!(given_value, Value::Null);
        }
    }
    false
}

fn comparison(expr: &Expr) -> Option<(&Expr, BinaryOp, &Value)> {
    let Expr::Binary { op, lhs, rhs } = expr else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return None;
    }
    if let Expr::Literal(value) = rhs.as_ref() {
        return Some((lhs, *op, value));
    }
    if let Expr::Literal(value) = lhs.as_ref() {
        let reversed = match op {
            BinaryOp::Less => BinaryOp::Greater,
            BinaryOp::LessEqual => BinaryOp::GreaterEqual,
            BinaryOp::Greater => BinaryOp::Less,
            BinaryOp::GreaterEqual => BinaryOp::LessEqual,
            other => *other,
        };
        return Some((rhs, reversed, value));
    }
    None
}

fn comparison_implies(given: BinaryOp, left: &Value, required: BinaryOp, right: &Value) -> bool {
    use BinaryOp::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return false;
    }
    if std::mem::discriminant(left) != std::mem::discriminant(right) {
        return false;
    }
    let order = left.cmp(right);
    match (given, required) {
        (Equal, Equal) | (NotEqual, NotEqual) => order.is_eq(),
        (Equal, NotEqual) => !order.is_eq(),
        (Equal | GreaterEqual, Greater) | (GreaterEqual, NotEqual) => order.is_gt(),
        (Equal | LessEqual, Less) | (LessEqual, NotEqual) => order.is_lt(),
        (Equal | Greater | GreaterEqual, GreaterEqual) | (Greater, Greater | NotEqual) => {
            order.is_ge()
        }
        (Equal | Less | LessEqual, LessEqual) | (Less, Less | NotEqual) => order.is_le(),
        _ => false,
    }
}

pub(in crate::sql) fn validate_conflict_target(
    engine: &Engine,
    table: &str,
    conflict: &ConflictPlan,
) -> Result<(), SQLError> {
    let keys = engine
        .enforced_keys(table)
        .map_err(|error| SQLError::Internal(format!("conflict target keys: {error}")))?;
    conflict_key_indices(engine, table, &keys, conflict).map(|_| ())
}

fn constraint_target_index(
    engine: &Engine,
    table: &str,
    keys: &[EnforcedKey],
    name: &str,
) -> Result<Vec<usize>, SQLError> {
    if let Some(index) = keys
        .iter()
        .position(|key| key.constraint_owned && key.name.as_deref() == Some(name))
    {
        return Ok(vec![index]);
    }
    let snapshot = engine
        .try_declared_table_constraints(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let exists = snapshot
        .checks
        .iter()
        .any(|check| check.name.as_deref() == Some(name))
        || snapshot
            .foreign_keys
            .iter()
            .any(|key| key.name.as_deref() == Some(name))
        || columns.iter().any(|column| {
            column.not_null_name.as_deref() == Some(name)
                || column.check_name.as_deref() == Some(name)
                || column
                    .references
                    .as_ref()
                    .is_some_and(|reference| reference.name.as_deref() == Some(name))
        });
    if exists {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: "constraint in ON CONFLICT clause has no associated index".into(),
        });
    }
    Err(SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("constraint \"{name}\" for table \"{table}\" does not exist"),
    })
}

fn validate_conflict_columns(
    engine: &Engine,
    table: &str,
    names: &[String],
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    for name in names {
        if !columns.iter().any(|column| column.name == *name)
            && !matches!(
                name.as_str(),
                "ctid" | "tableoid" | "xmin" | "xmax" | "cmin" | "cmax"
            )
        {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    Ok(())
}
