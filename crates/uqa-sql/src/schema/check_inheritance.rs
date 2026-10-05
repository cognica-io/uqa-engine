//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK definition merging at CREATE and ALTER inheritance boundaries.

use crate::ast::{ColumnDef, ColumnType, Expr, TableCheck};
use crate::SQLError;
use crate::ScalarExpr;
use uqa_core::Value;

pub fn bind_parent_check_columns(parent: &str, expr: &mut Expr) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(parent).map_err(SQLError::Internal)?;
    crate::schema::generated::bind_schema_column_references(expr, parent);
    crate::schema::generated::bind_schema_column_references(expr, &relation.name);
    Ok(())
}

pub fn same_check_expression(
    left: &Expr,
    right: &Expr,
    columns: &[ColumnDef],
) -> Result<bool, SQLError> {
    fn canonical(expression: &Expr, columns: &[ColumnDef]) -> Result<ScalarExpr, SQLError> {
        let mut scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
        let mut failure = None;
        crate::plan::rewrite_scalar_expression(&mut scalar, &mut |node| {
            let ScalarExpr::Cast { expr, ty } = node else {
                return;
            };
            let Ok(target) = ColumnType::from_sql_name(ty) else {
                return;
            };
            if let ScalarExpr::Column(name) = expr.as_ref() {
                if columns
                    .iter()
                    .any(|column| column.name == *name && column.ty == target)
                {
                    *node = *expr.clone();
                }
            } else if let ScalarExpr::Literal(value @ Value::Str(_)) = expr.as_ref() {
                // PostgreSQL resolves an unknown string to an integer constant during analysis. Keep wider and narrower integer coercions distinct from the ordinary int4 literal.
                if target == ColumnType::Integer {
                    match crate::expr::cast_value(value, ty) {
                        Ok(value) => *node = ScalarExpr::Literal(value),
                        Err(error) => failure = Some(error),
                    }
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(scalar)
    }
    Ok(canonical(left, columns)? == canonical(right, columns)?)
}

pub fn duplicate_check(table: &str, name: &str) -> SQLError {
    error(
        "42710",
        format!("constraint \"{name}\" for relation \"{table}\" already exists"),
    )
}

fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

/// The caller decides whether a local or inherited duplicate is eligible to merge. Existing validation and enforcement states follow `PostgreSQL`'s directional merge rules.
pub fn validate_check_merge(
    table: &str,
    existing: &TableCheck,
    incoming: &TableCheck,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    let name = incoming.name.as_deref().unwrap_or("<unnamed>");
    if !same_check_expression(&existing.expr, &incoming.expr, columns)? {
        return Err(duplicate_check(table, name));
    }
    let conflict = if existing.no_inherit {
        Some("non-inherited")
    } else if incoming.no_inherit {
        Some("inherited")
    } else if incoming.validated && existing.enforced && !existing.validated {
        Some("NOT VALID")
    } else if (!incoming.is_local && incoming.enforced && !existing.enforced)
        || (incoming.is_local && !incoming.enforced && existing.enforced)
    {
        Some("NOT ENFORCED")
    } else {
        None
    };
    if let Some(conflict) = conflict {
        return Err(error("42P17", format!("constraint \"{name}\" conflicts with {conflict} constraint on relation \"{table}\"")));
    }
    Ok(())
}

/// `MergeCheckConstraint`: a parent's CHECK joins the constraints the new table inherits, merging with an earlier parent's CHECK of the same name when their expressions match, an enforced copy making the merged constraint enforced.
pub fn merge_inherited_check(
    inherited: &mut Vec<TableCheck>,
    check: TableCheck,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    let Some(existing) = inherited
        .iter_mut()
        .find(|existing| existing.name.is_some() && existing.name == check.name)
    else {
        inherited.push(check);
        return Ok(());
    };
    if !same_check_expression(&existing.expr, &check.expr, columns)? {
        return Err(error(
            "42710",
            format!(
                "check constraint name \"{}\" appears multiple times but with different expressions",
                check.name.as_deref().unwrap_or("<unnamed>")
            ),
        ));
    }
    existing.enforced |= check.enforced;
    existing.validated = existing.enforced;
    Ok(())
}
