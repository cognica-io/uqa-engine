//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK definition merging at CREATE and ALTER inheritance boundaries.

use crate::ast::{ColumnDef, ColumnType, Expr, TableCheck};
use crate::ScalarExpr;
use crate::{ast::CreateTable, SQLError};
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

/// Merge bound CHECK expressions after the complete CREATE row type has been validated. Anonymous local constraints remain independent and receive names at publication.
pub fn merge_create_checks(table: &mut CreateTable) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&table.name).map_err(SQLError::Internal)?;
    let mut local_names = std::collections::BTreeSet::new();
    for name in table
        .columns
        .iter()
        .filter_map(|column| column.check_name.as_ref())
        .chain(
            table
                .checks
                .iter()
                .filter(|check| check.is_local)
                .filter_map(|check| check.name.as_ref()),
        )
    {
        if !local_names.insert(name) {
            return Err(duplicate_check(&relation.name, name));
        }
    }
    if table.hierarchy.parents.is_empty() {
        return Ok(());
    }
    let check_columns = table.columns.clone();
    let mut inherited: Vec<TableCheck> = Vec::new();
    let mut local = Vec::new();
    for check in std::mem::take(&mut table.checks) {
        if check.is_local {
            local.push(check);
        } else if let Some(existing) = inherited
            .iter_mut()
            .find(|existing| existing.name == check.name)
        {
            if !same_check_expression(&existing.expr, &check.expr, &check_columns)? {
                return Err(error("42710", format!("check constraint name \"{}\" appears multiple times but with different expressions", check.name.as_deref().unwrap_or("<unnamed>"))));
            }
            existing.enforced |= check.enforced;
            existing.validated = existing.enforced;
        } else {
            inherited.push(check);
        }
    }
    for column in &mut table.columns {
        let Some((expr, name)) = column.check.as_ref().zip(column.check_name.as_ref()) else {
            continue;
        };
        let Some(index) = inherited
            .iter()
            .position(|check| check.name.as_ref() == Some(name))
        else {
            continue;
        };
        let incoming = TableCheck {
            name: Some(name.clone()),
            object_id: column.check_object_id,
            is_local: true,
            expr: expr.clone(),
            enforced: column.check_enforced,
            validated: column.check_validated,
            no_inherit: column.check_no_inherit,
            partition_constraint: None,
        };
        validate_check_merge(&relation.name, &inherited[index], &incoming, &check_columns)?;
        inherited.remove(index);
        column.check_is_local = !table.hierarchy.is_partition();
    }
    for mut check in local {
        if let Some(index) = inherited
            .iter()
            .position(|existing| existing.name.is_some() && existing.name == check.name)
        {
            let existing = &inherited[index];
            if existing.is_local {
                return Err(duplicate_check(
                    &relation.name,
                    check.name.as_deref().unwrap_or("<unnamed>"),
                ));
            }
            validate_check_merge(&relation.name, existing, &check, &check_columns)?;
            check.is_local = !table.hierarchy.is_partition();
            inherited[index] = check;
        } else {
            inherited.push(check);
        }
    }
    table.checks = inherited;
    Ok(())
}
