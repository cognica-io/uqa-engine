//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed index-key preparation and column names assigned at index creation.

use crate::ast::{Expr, GeneratedColumnKind, IndexKey};
use crate::schema::SchemaBindingContext;
use crate::{ast::CreateIndex, ColumnType, SQLError};

pub fn key_names(keys: &[IndexKey]) -> Vec<String> {
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
            crate::parse_regobject_name(name)
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
                    crate::parse_regtype_name(ty)
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

pub fn require_column_key<'a>(key: &'a IndexKey, method: &str) -> Result<&'a str, SQLError> {
    key.column().ok_or_else(|| {
        SQLError::Unsupported(format!(
            "expression keys for access method `{method}` are not implemented"
        ))
    })
}

pub fn prepare_index_keys(
    context: &SchemaBindingContext<'_, '_>,
    statement: &mut CreateIndex,
) -> Result<Vec<ColumnType>, SQLError> {
    let definitions = context
        .catalog
        .schema_expression_columns(&statement.table)?
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
                        && crate::schema::dependencies::schema_expr_references_column(
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
                let ty = super::prepare_index_expression(
                    context.catalog,
                    context.binding,
                    &statement.table,
                    expression,
                )?;
                let column = match expression.as_ref() {
                    crate::ast::Expr::Column(name) => Some(name.clone()),
                    crate::ast::Expr::Cast { expr, .. } => {
                        if let crate::ast::Expr::Column(name) = expr.as_ref() {
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

/// Bind keys and predicates and retain the public attribute names assigned before expression simplification.
pub fn prepare_index_definition(
    catalog: &dyn crate::schema::SchemaExpressionCatalog,
    bindings: &dyn crate::semantics::conflict::InferenceBindingScope,
    c: &mut CreateIndex,
) -> Result<crate::catalog::index::IndexDefinition, SQLError> {
    let attribute_keys = c
        .columns
        .iter()
        .cloned()
        .chain(
            c.included_columns
                .iter()
                .cloned()
                .map(crate::ast::IndexKey::Column),
        )
        .collect::<Vec<_>>();
    let key_names = key_names(&attribute_keys);
    let binding = bindings.binding_scope()?;
    let key_types = prepare_index_keys(
        &SchemaBindingContext {
            catalog,
            binding: &binding.context(),
        },
        c,
    )?;
    if let Some(predicate) = c.predicate.as_deref_mut() {
        let binding = bindings.binding_scope()?;
        crate::schema::indexes::prepare_index_predicate(
            catalog,
            &binding.context(),
            &c.table,
            predicate,
        )?;
    }
    Ok(crate::catalog::index::IndexDefinition {
        key_names,
        key_types,
        included_columns: c.included_columns.clone(),
        column_order: c.column_order.clone(),
        predicate: c.predicate.clone(),
        unique: c.unique,
        nulls_not_distinct: c.nulls_not_distinct,
    })
}
