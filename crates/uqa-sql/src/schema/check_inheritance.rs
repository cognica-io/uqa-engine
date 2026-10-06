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

/// Remove casts that parse analysis treats as an identity before storing or comparing CHECK syntax.
pub(super) fn remove_identity_casts(
    expression: &mut Expr,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    crate::catalog::stored_ast::visit_stored_expression(expression, &mut |node| {
        while let Expr::Cast { expr, ty } = node {
            let Ok(target) = ColumnType::from_sql_name(ty) else {
                break;
            };
            let source = match expr.as_ref() {
                Expr::Column(name) => columns
                    .iter()
                    .find(|column| column.name == *name)
                    .map(|column| column.ty.clone()),
                Expr::TypedLiteral { ty, .. } => ColumnType::from_sql_name(ty).ok(),
                Expr::Literal(_) => crate::scalar_type(
                    &crate::plan::ExpressionPlan::lower(*expr.clone()).scalar,
                    &crate::RowSchema::default(),
                    &[],
                )?,
                _ => None,
            };
            if source.as_ref() != Some(&target) {
                break;
            }
            *node = *expr.clone();
        }
        Ok(())
    })
}

pub fn same_check_expression(
    left: &Expr,
    right: &Expr,
    columns: &[ColumnDef],
) -> Result<bool, SQLError> {
    fn canonical(expression: &Expr, columns: &[ColumnDef]) -> Result<ScalarExpr, SQLError> {
        let mut expression = expression.clone();
        remove_identity_casts(&mut expression, columns)?;
        let mut scalar = crate::plan::ExpressionPlan::lower(expression).scalar;
        let mut failure = None;
        crate::plan::rewrite_scalar_expression(&mut scalar, &mut |node| {
            if let ScalarExpr::TypedLiteral {
                value: Value::Int(value),
                ty,
                parameter_index: None,
                ..
            } = node
            {
                if matches!(ColumnType::from_sql_name(ty), Ok(ColumnType::Integer))
                    && i32::try_from(*value).is_ok()
                {
                    *node = ScalarExpr::Literal(Value::Int(*value));
                }
                return;
            }
            let ScalarExpr::Cast { expr, ty, .. } = node else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_checks_compare_cooked_int4_constants_without_erasing_other_types() {
        let untyped = Expr::Literal(Value::Int(0));
        for ty in ["integer", "int4"] {
            let cooked = Expr::TypedLiteral {
                value: Value::Int(0),
                ty: ty.into(),
            };
            assert!(same_check_expression(&untyped, &cooked, &[]).unwrap());
            assert!(same_check_expression(&cooked, &untyped, &[]).unwrap());
            let mut cast = Expr::Cast {
                expr: Box::new(cooked.clone()),
                ty: "integer".into(),
            };
            assert!(same_check_expression(&cast, &untyped, &[]).unwrap());
            remove_identity_casts(&mut cast, &[]).unwrap();
            assert_eq!(cast, cooked);
        }
        for ty in ["smallint", "bigint", "oid"] {
            let cooked = Expr::TypedLiteral {
                value: Value::Int(0),
                ty: ty.into(),
            };
            assert!(!same_check_expression(&untyped, &cooked, &[]).unwrap());
        }
    }
}
