//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 generated-column validation and row computation.

use super::{
    aggregates, convert_value_to_column_type, scalar::eval_lowered_expression, ColumnType, Engine,
    ForeignKey, SQLError,
};
use uqa_sql::ast::{ColumnDef, Expr, GeneratedColumnKind, TableKeyConstraint};
use uqa_storage::document_store::Document;

mod typing;

pub(in crate::sql) fn prepare_generated_columns(
    engine: &Engine,
    table: &str,
    columns: &mut [ColumnDef],
    key_constraints: &[TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    let snapshot = columns.to_vec();
    for (index, column) in snapshot.iter().enumerate() {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if column.default.is_some() {
            return Err(SQLError::TypeMismatch(format!(
                "both default and generation expression specified for column `{}`",
                column.name
            )));
        }
        if column.auto_increment {
            return Err(SQLError::TypeMismatch(format!(
                "both identity and generation expression specified for column `{}`",
                column.name
            )));
        }
        if generated.kind == GeneratedColumnKind::Virtual {
            validate_virtual_column_envelope(column, key_constraints, foreign_keys)?;
        }
        let plan = uqa_planner::ExpressionPlan::lower((*generated.expression).clone());
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
        validate_generation_expression(
            engine,
            table,
            &snapshot,
            &generated.expression,
            generated.kind,
        )?;
        let prepared = columns[index]
            .generated
            .as_mut()
            .ok_or_else(|| SQLError::Internal("generated column disappeared".into()))?;
        let (expression_type, function_dependencies) =
            typing::infer_generation_expression(engine, &snapshot, &mut prepared.expression)?;
        if let typing::GenerationType::UnknownLiteral(value) = &expression_type {
            convert_value_to_column_type(uqa_core::Value::Str(value.clone()), &column.ty)?;
        } else if !typing::generation_type_assignable_to(&expression_type, &column.ty) {
            return Err(SQLError::TypeMismatch(format!(
                "column `{}` has type {} but generation expression has type {}",
                column.name,
                super::column_type_name(&column.ty),
                typing::generation_type_name(&expression_type)
            )));
        }
        prepared.function_dependencies = function_dependencies;
    }
    Ok(())
}

fn validate_virtual_column_envelope(
    column: &ColumnDef,
    key_constraints: &[TableKeyConstraint],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    if contains_engine_defined_type(&column.ty) {
        return Err(SQLError::TypeMismatch(format!(
            "virtual generated column `{}` cannot use a user-defined type",
            column.name
        )));
    }
    if column.primary_key
        || key_constraints.iter().any(|constraint| {
            constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey
                && constraint.columns.iter().any(|name| name == &column.name)
        })
    {
        return Err(SQLError::TypeMismatch(
            "primary keys on virtual generated columns are not supported".into(),
        ));
    }
    if column.unique
        || key_constraints.iter().any(|constraint| {
            constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
                && constraint.columns.iter().any(|name| name == &column.name)
        })
    {
        return Err(SQLError::TypeMismatch(
            "unique constraints on virtual generated columns are not supported".into(),
        ));
    }
    if column.references.is_some()
        || foreign_keys.iter().any(|foreign_key| {
            foreign_key
                .local_columns
                .iter()
                .any(|name| name == &column.name)
        })
    {
        return Err(SQLError::TypeMismatch(
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
    engine: &Engine,
    table: &str,
    columns: &[ColumnDef],
    expression: &Expr,
    kind: GeneratedColumnKind,
) -> Result<(), SQLError> {
    match expression {
        Expr::Column(name) => validate_generation_column_reference(columns, name),
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if !relation_qualifier_matches(table, qualifier) {
                return Err(SQLError::UnknownTable(qualifier.clone()));
            }
            validate_generation_column_reference(columns, column)
        }
        Expr::Func {
            name,
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
            if kind == GeneratedColumnKind::Virtual
                && (engine
                    .registered_runtime_function_volatility(name)
                    .is_some()
                    || engine.lookup_sql_functions(name).is_some())
            {
                return Err(SQLError::TypeMismatch(
                    "generation expression uses user-defined function; virtual generated columns cannot use user-defined functions"
                        .into(),
                ));
            }
            for argument in args {
                validate_generation_expression(engine, table, columns, argument, kind)?;
            }
            Ok(())
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                validate_generation_expression(engine, table, columns, item, kind)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            validate_generation_expression(engine, table, columns, lhs, kind)?;
            validate_generation_expression(engine, table, columns, rhs, kind)
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            validate_generation_expression(engine, table, columns, inner, kind)
        }
        Expr::Between { expr, low, high } => {
            validate_generation_expression(engine, table, columns, expr, kind)?;
            validate_generation_expression(engine, table, columns, low, kind)?;
            validate_generation_expression(engine, table, columns, high, kind)
        }
        Expr::InList { expr, list, .. } => {
            validate_generation_expression(engine, table, columns, expr, kind)?;
            for item in list {
                validate_generation_expression(engine, table, columns, item, kind)?;
            }
            Ok(())
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                validate_generation_expression(engine, table, columns, base, kind)?;
            }
            for (condition, result) in when {
                validate_generation_expression(engine, table, columns, condition, kind)?;
                validate_generation_expression(engine, table, columns, result, kind)?;
            }
            if let Some(else_branch) = else_branch {
                validate_generation_expression(engine, table, columns, else_branch, kind)?;
            }
            Ok(())
        }
        Expr::Default | Expr::Param(_) => Err(SQLError::TypeMismatch(
            "parameters and DEFAULT are not allowed in column generation expressions".into(),
        )),
        Expr::Star => Err(SQLError::TypeMismatch(
            "whole-row references are not allowed in column generation expressions".into(),
        )),
        Expr::WindowCall { .. } => Err(SQLError::TypeMismatch(
            "window functions are not allowed in column generation expressions".into(),
        )),
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => Err(
            SQLError::TypeMismatch("cannot use subquery in column generation expression".into()),
        ),
        Expr::Literal(_) => Ok(()),
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

fn relation_qualifier_matches(table: &str, qualifier: &str) -> bool {
    table == qualifier
        || table
            .rsplit_once('.')
            .is_some_and(|(_, local)| local == qualifier)
        || qualifier
            .rsplit_once('.')
            .is_some_and(|(_, local)| local == table)
}

pub(crate) fn refresh_stored_generated_columns(
    engine: &Engine,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read generated columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    for column in &columns {
        if column.generated.is_some() {
            document.remove(&column.name);
        }
    }
    for column in &columns {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if generated.kind != GeneratedColumnKind::Stored {
            continue;
        }
        let value = eval_lowered_expression(engine, &generated.expression, Some(document), &[])?;
        document.insert(
            column.name.clone(),
            convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

pub(in crate::sql) fn generated_column_kind(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<Option<GeneratedColumnKind>, SQLError> {
    Ok(engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read generated column: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .into_iter()
        .find(|definition| definition.name == column)
        .and_then(|definition| definition.generated.map(|generated| generated.kind)))
}
