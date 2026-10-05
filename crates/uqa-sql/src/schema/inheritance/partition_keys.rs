//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` partition key validation. `transformPartitionSpec` analyzes every key expression first; `ComputePartitionAttrs` then checks each key in order for missing, system and generated columns, mutable functions, and constant expressions.

use super::InheritanceContext;
use crate::ast::{ColumnDef, CreateTable, Expr, PartitionStrategy};
use crate::ir::ScalarExpr;
use crate::schema::columns::POSTGRES_SYSTEM_COLUMNS;
use crate::semantics::partition::validate_hash_partition_spec;
use crate::SQLError;
use std::collections::BTreeSet;

/// `PARTITION_MAX_KEYS`.
const PARTITION_MAX_KEYS: usize = 32;

pub(super) fn validate_partition_keys(
    context: &InheritanceContext<'_>,
    table: &CreateTable,
) -> Result<(), SQLError> {
    let Some(spec) = table.hierarchy.partition_spec.as_ref() else {
        return Ok(());
    };
    if spec.keys.len() > PARTITION_MAX_KEYS {
        return Err(error(
            "54011",
            &format!("cannot partition using more than {PARTITION_MAX_KEYS} columns"),
        ));
    }
    // `transformPartitionSpec` counts the key columns before it resolves any of them.
    if spec.strategy == PartitionStrategy::List && spec.keys.len() != 1 {
        return Err(error(
            "42P17",
            "cannot use \"list\" partition strategy with more than one column",
        ));
    }
    for key in &spec.keys {
        if !matches!(key, Expr::Column(_)) {
            let plan = crate::plan::ExpressionPlan::lower(key.clone());
            analyze_key_expression(context, &plan.scalar, &table.columns)?;
        }
    }
    for key in &spec.keys {
        match key {
            Expr::Column(name) => check_column_key(&table.columns, name)?,
            expression => check_expression_key(context, &table.columns, expression)?,
        }
    }
    validate_hash_partition_spec(&context.partitions, spec, &table.columns)?;
    for key in &spec.keys {
        crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
            context.roles,
            key,
            None,
        )?;
    }
    Ok(())
}

/// `EXPR_KIND_PARTITION_EXPRESSION` analysis: unknown columns, then aggregates, window functions and set-returning functions, with a call's arguments analyzed before the call.
fn analyze_key_expression(
    context: &InheritanceContext<'_>,
    expression: &ScalarExpr,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    match expression {
        ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. }
            if !columns.iter().any(|column| column.name == *name)
                && !POSTGRES_SYSTEM_COLUMNS.contains(&name.as_str()) =>
        {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            return Err(error(
                "0A000",
                "cannot use subquery in partition key expression",
            ));
        }
        _ => {}
    }
    let mut root = true;
    expression.try_visit(&mut |node| {
        if std::mem::take(&mut root) {
            return Ok(true);
        }
        analyze_key_expression(context, node, columns)?;
        Ok::<_, SQLError>(false)
    })?;
    let catalog = context.partitions.schema;
    match expression {
        ScalarExpr::WindowCall { .. } => Err(error(
            "42P20",
            "window functions are not allowed in partition key expressions",
        )),
        ScalarExpr::Func { .. }
            if crate::semantics::aggregates::is_aggregate(catalog, expression) =>
        {
            Err(error(
                "42803",
                "aggregate functions are not allowed in partition key expressions",
            ))
        }
        ScalarExpr::Func { .. }
            if crate::semantics::sets::validation::expression_may_return_set(
                catalog,
                catalog,
                expression,
                &row_schema(columns),
                &[],
            )? =>
        {
            Err(error(
                "0A000",
                "set-returning functions are not allowed in partition key expressions",
            ))
        }
        _ => Ok(()),
    }
}

fn check_column_key(columns: &[ColumnDef], name: &str) -> Result<(), SQLError> {
    if POSTGRES_SYSTEM_COLUMNS.contains(&name) {
        return Err(error(
            "42P17",
            &format!("cannot use system column \"{name}\" in partition key"),
        ));
    }
    let column = columns
        .iter()
        .find(|column| column.name == name)
        .ok_or_else(|| {
            error(
                "42703",
                &format!("column \"{name}\" named in partition key does not exist"),
            )
        })?;
    if column.generated.is_some() {
        return Err(generated_column_key(name));
    }
    Ok(())
}

fn check_expression_key(
    context: &InheritanceContext<'_>,
    columns: &[ColumnDef],
    expression: &Expr,
) -> Result<(), SQLError> {
    let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
    let mut referenced = BTreeSet::new();
    scalar.collect_columns(&mut referenced);
    if referenced
        .iter()
        .any(|name| POSTGRES_SYSTEM_COLUMNS.contains(&name.as_str()))
    {
        return Err(error(
            "42P17",
            "partition key expressions cannot contain system column references",
        ));
    }
    for name in &referenced {
        if columns
            .iter()
            .any(|column| column.name == *name && column.generated.is_some())
        {
            return Err(generated_column_key(name));
        }
    }
    let mut typed = expression.clone();
    crate::schema::generated::typing::infer_generation_expression(
        context.partitions.schema,
        columns,
        &mut typed,
    )
    .map_err(|error| {
        if error.sqlstate() == Some("42P17") {
            self::error(
                "42P17",
                "functions in partition key expression must be marked IMMUTABLE",
            )
        } else {
            error
        }
    })?;
    // An immutable expression over no column folds to a constant, which PostgreSQL rejects as a key.
    if referenced.is_empty() {
        return Err(error(
            "42P17",
            "cannot use constant expression as partition key",
        ));
    }
    Ok(())
}

fn generated_column_key(name: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P17".into(),
        message: "cannot use generated column in partition key".into(),
        detail: Some(format!("Column \"{name}\" is a generated column.")),
        hint: None,
    }
}

fn row_schema(columns: &[ColumnDef]) -> crate::RowSchema {
    crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    )
}

fn error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
