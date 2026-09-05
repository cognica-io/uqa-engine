//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed index-key preparation and column names assigned at index creation.

use super::{ColumnType, CreateIndex, Engine, SQLError};
use uqa_sql::ast::{Expr, GeneratedColumnKind, IndexKey};

pub(super) fn key_names(keys: &[IndexKey]) -> Vec<String> {
    let mut names = Vec::with_capacity(keys.len());
    for key in keys {
        let label = match key {
            IndexKey::Column(column) => column.clone(),
            IndexKey::Expression(expression) => {
                expression_name(expression).map_or_else(|| "expr".into(), |(name, _)| name)
            }
        };
        let mut name = label.clone();
        let mut suffix = 1_u64;
        while names.contains(&name) {
            name = format!("{label}{suffix}");
            suffix += 1;
        }
        names.push(name);
    }
    names
}

fn expression_name(expression: &Expr) -> Option<(String, bool)> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            Some((name.clone(), true))
        }
        Expr::Func { name, .. } => Some((
            uqa_sql::parse_regobject_name(name)
                .and_then(|mut names| names.pop())
                .unwrap_or_else(|| name.clone()),
            true,
        )),
        Expr::Cast { expr, ty } => {
            let inner = expression_name(expr);
            if inner.as_ref().is_some_and(|(_, strong)| *strong) {
                inner
            } else {
                Some((
                    uqa_sql::parse_regtype_name(ty)
                        .ok()
                        .flatten()
                        .and_then(|mut name| name.names.pop())
                        .unwrap_or_else(|| ty.clone()),
                    false,
                ))
            }
        }
        Expr::Case { else_branch, .. } => {
            let inner = else_branch.as_deref().and_then(expression_name);
            Some(
                inner
                    .filter(|(_, strong)| *strong)
                    .unwrap_or_else(|| ("case".into(), false)),
            )
        }
        Expr::Array(_) => Some(("array".into(), true)),
        Expr::Row(_) => Some(("row".into(), true)),
        _ => None,
    }
}

pub(super) fn require_column_key<'a>(key: &'a IndexKey, method: &str) -> Result<&'a str, SQLError> {
    key.column().ok_or_else(|| {
        SQLError::Unsupported(format!(
            "expression keys for access method `{method}` are not implemented"
        ))
    })
}

pub(super) fn prepare_index_keys(
    engine: &Engine,
    statement: &mut CreateIndex,
) -> Result<Vec<ColumnType>, SQLError> {
    let definitions = engine
        .try_describe_table(&statement.table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(statement.table.clone()))?;
    let mut types = Vec::with_capacity(statement.columns.len());
    for key in &mut statement.columns {
        match key {
            IndexKey::Column(name) => {
                let Some(column) = definitions.iter().find(|column| column.name == *name) else {
                    if definitions.is_empty() {
                        types.push(ColumnType::Text);
                        continue;
                    }
                    return Err(SQLError::UnknownColumn(name.clone()));
                };
                if column
                    .generated
                    .as_ref()
                    .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
                {
                    return Err(SQLError::Unsupported(format!(
                        "indexes on virtual generated column `{name}` are not supported"
                    )));
                }
                types.push(column.ty.clone());
            }
            IndexKey::Expression(expression) => {
                for column in &definitions {
                    if column
                        .generated
                        .as_ref()
                        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
                        && crate::engine_table_storage::schema_expr_references_column(
                            expression,
                            &column.name,
                        )
                    {
                        return Err(SQLError::Unsupported(format!(
                            "index expressions cannot use virtual generated column `{}`",
                            column.name
                        )));
                    }
                }
                let ty = crate::sql::generated::prepare_index_expression(
                    engine,
                    &statement.table,
                    expression,
                )?;
                let column = match expression.as_ref() {
                    uqa_sql::ast::Expr::Column(name) => Some(name.clone()),
                    uqa_sql::ast::Expr::Cast { expr, .. } => {
                        if let uqa_sql::ast::Expr::Column(name) = expr.as_ref() {
                            definitions
                                .iter()
                                .any(|column| column.name == *name && column.ty == ty)
                                .then(|| name.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(column) = column {
                    *key = IndexKey::Column(column);
                }
                types.push(ty);
            }
        }
    }
    let mut included = std::collections::BTreeSet::new();
    for name in &statement.included_columns {
        if !definitions.is_empty() && !definitions.iter().any(|column| column.name == *name) {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
        if !included.insert(name)
            || statement
                .columns
                .iter()
                .any(|key| key.column() == Some(name.as_str()))
        {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{name}\" included more than once"),
            });
        }
    }
    if !included.is_empty() && statement.access_method == "gin" {
        return Err(SQLError::Unsupported(
            "access method \"gin\" does not support included columns".into(),
        ));
    }
    Ok(types)
}
