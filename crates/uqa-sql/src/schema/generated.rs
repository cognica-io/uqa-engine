//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 generated-column validation and row computation.

use crate::ast::ForeignKey;
use crate::ast::{ColumnDef, Expr, GeneratedColumnKind};
use crate::{
    assignment::conversion::convert_value_to_column_type, semantics::aggregates, ColumnType,
    SQLError,
};

pub(crate) mod eligibility;
pub(super) mod typing;
mod virtual_security;

pub fn prepare_generated_columns(
    context: &super::SchemaBindingContext<'_, '_>,
    qualifier: &str,
    columns: &mut [ColumnDef],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    let snapshot = columns.to_vec();
    for index in 0..columns.len() {
        prepare_generated_column(context, qualifier, &snapshot, columns, index, foreign_keys)?;
    }
    Ok(())
}

/// Validate and bind the generation expression of `columns[index]`, if it has one, against the relation's columns as `snapshot` describes them before any of their expressions was bound.
pub fn prepare_generated_column(
    context: &super::SchemaBindingContext<'_, '_>,
    qualifier: &str,
    snapshot: &[ColumnDef],
    columns: &mut [ColumnDef],
    index: usize,
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    let engine = context.catalog;
    let column = &snapshot[index];
    let Some(generated) = column.generated.as_ref() else {
        return Ok(());
    };
    if column.default.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "both default and generation expression specified for column `{}`",
            column.name
        )));
    }
    if column.auto_increment.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "both identity and generation expression specified for column `{}`",
            column.name
        )));
    }
    if generated.kind == GeneratedColumnKind::Virtual {
        validate_virtual_column_envelope(column, foreign_keys)?;
    }
    let plan = crate::plan::ExpressionPlan::lower((*generated.expression).clone());
    if !plan.subqueries.is_empty() {
        return Err(SQLError::TypeMismatch(
            "cannot use subquery in column generation expression".into(),
        ));
    }
    if aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(SQLError::TypeMismatch(
            "aggregate functions are not allowed in column generation expressions".into(),
        ));
    }
    validate_generation_expression(qualifier, snapshot, &generated.expression)?;
    if generated.kind == GeneratedColumnKind::Virtual {
        virtual_security::check_virtual_host_functions(engine, &generated.expression)?;
    }
    let prepared = columns[index]
        .generated
        .as_mut()
        .ok_or_else(|| SQLError::Internal("generated column disappeared".into()))?;
    bind_schema_column_references(&mut prepared.expression, qualifier);
    let (expression_type, function_dependencies) =
        typing::infer_generation_expression(engine, snapshot, &mut prepared.expression)?;
    if generated.kind == GeneratedColumnKind::Virtual {
        virtual_security::check_virtual_generated_security(engine, snapshot, &prepared.expression)?;
    }
    crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
        engine,
        &prepared.expression,
        Some(&column.ty),
    )?;
    if let typing::GenerationType::UnknownLiteral(value) = &expression_type {
        convert_value_to_column_type(uqa_core::Value::Str(value.clone()), &column.ty)?;
    } else if !typing::generation_type_assignable_to(&expression_type, &column.ty) {
        return Err(SQLError::TypeMismatch(format!(
            "column `{}` has type {} but generation expression has type {}",
            column.name,
            crate::catalog::type_metadata::column_type_name(&column.ty),
            typing::generation_type_name(&expression_type)
        )));
    }
    prepared.function_dependencies = function_dependencies;
    // The generation result is assigned to the column; its routines, user-defined types and enum constants are stored by identity as parse analysis stores them.
    crate::catalog::stored_ast::fold_assigned_stored_literal(
        &mut prepared.expression,
        &column.ty,
        crate::FunctionTypeResolver::enum_labels(engine),
    )?;
    super::constraints::bind_stored_check_expression(
        context,
        qualifier,
        qualifier,
        snapshot,
        &mut prepared.expression,
    )?;
    Ok(())
}

/// A virtual generated column cannot take a user-defined type or a foreign key; `DefineIndex` rejects it as a key column, after the key's other checks.
fn validate_virtual_column_envelope(
    column: &ColumnDef,
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    if virtual_security::is_user_defined_type(&column.ty) {
        return Err(SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: format!(
                "virtual generated column \"{}\" cannot have a user-defined type",
                column.name
            ),
            detail: Some(virtual_security::USER_DEFINED_TYPE_DETAIL.into()),
            hint: None,
        });
    }
    if contains_engine_defined_type(&column.ty) {
        return Err(SQLError::TypeMismatch(format!(
            "virtual generated column `{}` cannot use a user-defined type",
            column.name
        )));
    }
    if column.references.is_some()
        || foreign_keys.iter().any(|foreign_key| {
            foreign_key
                .local_columns
                .iter()
                .any(|name| name == &column.name)
        })
    {
        return Err(SQLError::Unsupported(
            "foreign key constraints on virtual generated columns are not supported".into(),
        ));
    }
    Ok(())
}

fn contains_engine_defined_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Vector(_) | ColumnType::Tensor(_) => true,
        ColumnType::Array(element) => contains_engine_defined_type(element),
        _ => false,
    }
}

fn validate_generation_expression(
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &Expr,
) -> Result<(), SQLError> {
    match expression {
        Expr::Column(name) => validate_generation_column_reference(columns, name),
        Expr::QualifiedColumn {
            qualifier: expression_qualifier,
            column,
            ..
        } => {
            if expression_qualifier != qualifier {
                return Err(SQLError::UnknownTable(expression_qualifier.clone()));
            }
            validate_generation_column_reference(columns, column)
        }
        Expr::Func {
            args,
            distinct,
            order_by,
            filter,
            ..
        } => {
            if *distinct || !order_by.is_empty() || filter.is_some() {
                return Err(SQLError::TypeMismatch(
                    "aggregate syntax is not allowed in column generation expressions".into(),
                ));
            }
            for argument in args {
                validate_generation_expression(qualifier, columns, argument)?;
            }
            Ok(())
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                validate_generation_expression(qualifier, columns, item)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            validate_generation_expression(qualifier, columns, lhs)?;
            validate_generation_expression(qualifier, columns, rhs)
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            validate_generation_expression(qualifier, columns, inner)
        }
        Expr::Between { expr, low, high } => {
            validate_generation_expression(qualifier, columns, expr)?;
            validate_generation_expression(qualifier, columns, low)?;
            validate_generation_expression(qualifier, columns, high)
        }
        Expr::InList { expr, list, .. } => {
            validate_generation_expression(qualifier, columns, expr)?;
            for item in list {
                validate_generation_expression(qualifier, columns, item)?;
            }
            Ok(())
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                validate_generation_expression(qualifier, columns, base)?;
            }
            for (condition, result) in when {
                validate_generation_expression(qualifier, columns, condition)?;
                validate_generation_expression(qualifier, columns, result)?;
            }
            if let Some(else_branch) = else_branch {
                validate_generation_expression(qualifier, columns, else_branch)?;
            }
            Ok(())
        }
        Expr::Default | Expr::Param(_) => Err(SQLError::TypeMismatch(
            "parameters and DEFAULT are not allowed in column generation expressions".into(),
        )),
        Expr::Star | Expr::QualifiedStar(_) => Err(SQLError::TypeMismatch(
            "whole-row references are not allowed in column generation expressions".into(),
        )),
        Expr::InternalColumn(_) => Err(SQLError::Internal(
            "executor-only column reached generation expression validation".into(),
        )),
        Expr::WindowCall { .. } => Err(SQLError::TypeMismatch(
            "window functions are not allowed in column generation expressions".into(),
        )),
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => Err(
            SQLError::TypeMismatch("cannot use subquery in column generation expression".into()),
        ),
        Expr::Literal(_) | Expr::TypedLiteral { .. } => Ok(()),
    }
}

pub fn bind_schema_column_references(expression: &mut Expr, qualifier: &str) {
    if let Expr::QualifiedColumn {
        qualifier: expression_qualifier,
        column,
    } = expression
    {
        if expression_qualifier == qualifier {
            *expression = Expr::Column(column.clone());
        }
        return;
    }
    match expression {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                bind_schema_column_references(argument, qualifier);
            }
            for order in order_by {
                bind_schema_column_references(&mut order.expr, qualifier);
            }
            if let Some(filter) = filter {
                bind_schema_column_references(filter, qualifier);
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                bind_schema_column_references(item, qualifier);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            bind_schema_column_references(lhs, qualifier);
            bind_schema_column_references(rhs, qualifier);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            bind_schema_column_references(inner, qualifier);
        }
        Expr::Between { expr, low, high } => {
            bind_schema_column_references(expr, qualifier);
            bind_schema_column_references(low, qualifier);
            bind_schema_column_references(high, qualifier);
        }
        Expr::InList { expr, list, .. } => {
            bind_schema_column_references(expr, qualifier);
            for item in list {
                bind_schema_column_references(item, qualifier);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                bind_schema_column_references(base, qualifier);
            }
            for (condition, result) in when {
                bind_schema_column_references(condition, qualifier);
                bind_schema_column_references(result, qualifier);
            }
            if let Some(else_branch) = else_branch {
                bind_schema_column_references(else_branch, qualifier);
            }
        }
        Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => {}
    }
}

fn validate_generation_column_reference(columns: &[ColumnDef], name: &str) -> Result<(), SQLError> {
    let Some(column) = columns.iter().find(|column| column.name == name) else {
        return Err(SQLError::UnknownColumn(name.to_string()));
    };
    if column.generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "cannot use generated column `{name}` in column generation expression"
        )));
    }
    Ok(())
}
