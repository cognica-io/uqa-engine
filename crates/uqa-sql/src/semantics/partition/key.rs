//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partition key columns as `PostgreSQL` reports them: each key's declared type, the name used by bound coercion errors, and the spelling used by failing-row descriptions.

use crate::ast::{ColumnDef, ColumnType, Expr, PartitionSpec};
use crate::catalog::expression_text::schema_expr_text;
use crate::type_resolution::FunctionTypeResolver;
use crate::SQLError;

/// One partition key position.
pub(super) struct KeyColumn {
    /// Column name, or the deparsed key expression, as `transformPartitionBound` names it.
    pub(super) name: String,
    /// `pg_get_partkeydef` spelling: a quoted column name, or the expression in parentheses unless it is a function call.
    pub(super) description: String,
    pub(super) expression: bool,
    pub(super) ty: ColumnType,
}

pub(super) fn key_columns(
    resolver: &dyn FunctionTypeResolver,
    spec: &PartitionSpec,
    columns: &[ColumnDef],
) -> Result<Vec<KeyColumn>, SQLError> {
    spec.keys
        .iter()
        .map(|key| {
            let ty = key_type(resolver, key, columns)?;
            Ok(match key {
                Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => KeyColumn {
                    name: name.clone(),
                    description: crate::expr::quote_ident(name),
                    expression: false,
                    ty,
                },
                expression => {
                    let text = schema_expr_text(expression)?;
                    let description =
                        if matches!(expression, Expr::Func { .. }) || text.starts_with('(') {
                            text.clone()
                        } else {
                            format!("({text})")
                        };
                    KeyColumn {
                        name: text,
                        description,
                        expression: true,
                        ty,
                    }
                }
            })
        })
        .collect()
}

/// Declared type of one partition key: a column's type, or the resolved type of a key expression over the partitioned table's columns.
pub(super) fn key_type(
    resolver: &dyn FunctionTypeResolver,
    expression: &Expr,
    columns: &[ColumnDef],
) -> Result<ColumnType, SQLError> {
    if let Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } = expression {
        return columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column.ty.clone())
            .ok_or_else(|| SQLError::UnknownColumn(name.clone()));
    }
    let schema = crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let expression = crate::plan::ExpressionPlan::lower(expression.clone());
    crate::type_resolution::common_context_expression_type(
        &expression.scalar,
        &schema,
        &[],
        Some(resolver),
    )?
    .ok_or_else(|| SQLError::TypeMismatch("cannot determine partition key type".into()))
}
