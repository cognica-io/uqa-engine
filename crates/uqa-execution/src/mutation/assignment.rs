//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate mutation defaults, typed assignments and view check options.
mod subscripts;

use super::{
    errors::dml_storage_error,
    expressions::eval_mutation_expr,
    rows::context::{MutationExpressionContext, MutationRowContext},
};
use crate::{
    query::{locking::context::RowLockScopeSource, CteScope},
    OwnedPhysicalRow,
};
use uqa_core::{DocId, RelationIdentity, Value};
use uqa_sql::{
    assignment::{columns::AssignmentColumnCatalog, AssignmentContext},
    plan::{UpdatePlan, ViewCheckPlan},
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_storage::document_store::Document;
#[derive(Clone)]
pub struct MutationAssignmentContext<'a, S: Clone + 'static> {
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub assignment: &'a dyn AssignmentContext,
    pub rows: MutationRowContext<'a>,
    pub expressions: MutationExpressionContext<'a, S>,
    pub scopes: &'a dyn RowLockScopeSource<S>,
}
impl<S: Clone + 'static> Copy for MutationAssignmentContext<'_, S> {}
#[derive(Clone, Copy)]
pub struct MutationAssignmentTarget<'a> {
    pub table: &'a str,
    pub target: &'a uqa_sql::ast::AssignmentTarget<ScalarExpr>,
    pub current: Option<&'a Value>,
    pub final_column_write: bool,
    pub action: &'a str,
}

pub fn eval_mutation_assignment<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let MutationAssignmentTarget {
        table,
        target: assignment_target,
        action,
        ..
    } = target;
    let column = &assignment_target.column;
    let generated =
        uqa_sql::assignment::columns::generated_column_kind(services.columns, table, column)?;
    if matches!(expression, ScalarExpr::Default) {
        uqa_sql::assignment::targets::validate_assignment_default(assignment_target)?;
        if generated.is_some() {
            return Ok(None);
        }
        let value = match services
            .columns
            .try_column_insert_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?
        {
            Some(default) => crate::query::catalog_expression::eval_lowered_expression(
                services.expressions.expressions,
                services.scopes.current_routine_scope(),
                &default,
                None,
                params,
            )?,
            None => Value::Null,
        };
        return uqa_sql::assignment::columns::coerce_to_column_type(
            services.assignment,
            services.columns,
            table,
            column,
            value,
        )
        .map(Some);
    }
    if generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` is a generated column; only DEFAULT may be assigned"
        )));
    }
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    let hook = services.expressions.expressions.bind_scope(ctes.clone());
    let source = crate::scalar_type_with_resolver(expression, schema, params, hook.as_ref())?;
    subscripts::assign_value(
        services,
        ctes,
        target,
        || eval_mutation_expr(services.expressions, ctes, expression, row, params),
        source.as_ref(),
        row,
        params,
    )
    .map(Some)
}

/// Apply a value from INSERT SELECT using its declared source type and the original bound scope.
pub fn coerce_mutation_assignment<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: MutationAssignmentTarget<'_>,
    value: Value,
    source: Option<&uqa_sql::ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    subscripts::assign_value(services, ctes, target, || Ok(value), source, row, params)
}

#[derive(Clone, Copy)]
pub struct TypedAssignmentTarget<'a> {
    pub target: &'a uqa_sql::ast::AssignmentTarget<ScalarExpr>,
    pub ty: Option<&'a uqa_sql::ColumnType>,
    pub current: Option<&'a Value>,
    pub final_column_write: bool,
}

pub fn eval_typed_assignment<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: TypedAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let default = matches!(expression, ScalarExpr::Default);
    if default {
        uqa_sql::assignment::targets::validate_assignment_default(target.target)?;
    }
    uqa_sql::assignment::targets::validate_assignment_type(target.target, target.ty)?;
    let schema = RowSchema::default();
    let hook = services.expressions.expressions.bind_scope(ctes.clone());
    let source = crate::scalar_type_with_resolver(
        expression,
        row.map_or(&schema, |row| &row.schema),
        params,
        hook.as_ref(),
    )?;
    let value = || {
        if default {
            Ok(Value::Null)
        } else {
            eval_mutation_expr(services.expressions, ctes, expression, row, params)
        }
    };
    match target.ty {
        Some(ty) => subscripts::assign_typed_value(
            services,
            ctes,
            TypedAssignmentTarget {
                ty: Some(ty),
                ..target
            },
            value,
            source.as_ref(),
            row,
            params,
        ),
        None => value(),
    }
}

pub fn coerce_typed_assignment<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: TypedAssignmentTarget<'_>,
    value: Value,
    source: Option<&uqa_sql::ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    uqa_sql::assignment::targets::validate_assignment_type(target.target, target.ty)?;
    match target.ty {
        Some(ty) => subscripts::assign_typed_value(
            services,
            ctes,
            TypedAssignmentTarget {
                ty: Some(ty),
                ..target
            },
            || Ok(value),
            source,
            row,
            params,
        ),
        None => Ok(value),
    }
}

pub fn eval_view_rule_update_assignment<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    stmt: &UpdatePlan,
    assignment_position: usize,
    current: Option<&Value>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let assignment = &stmt.assignments[assignment_position];
    for plan in &stmt.view_rule_update_plans {
        let Some(column) = plan.assigned_columns.get(assignment_position) else {
            continue;
        };
        let schema = services.rows.relations.view_schema(&plan.relation)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    public.eq_ignore_ascii_case(column).then_some(position)
                })
        else {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                plan.relation, column
            )));
        };
        return eval_typed_assignment(
            services,
            ctes,
            TypedAssignmentTarget {
                target: &assignment.target,
                ty: schema.column_type(position),
                current,
                final_column_write: !stmt.assignments[assignment_position + 1..]
                    .iter()
                    .any(|next| next.target.column == assignment.target.column),
            },
            &assignment.value,
            row,
            params,
        )
        .map(Some);
    }
    eval_typed_assignment(
        services,
        ctes,
        TypedAssignmentTarget {
            target: &assignment.target,
            ty: None,
            current,
            final_column_write: true,
        },
        &assignment.value,
        row,
        params,
    )
    .map(Some)
}

pub struct ViewCheckContext<'a, S: Clone + 'static> {
    pub services: MutationAssignmentContext<'a, S>,
    pub table: &'a str,
    pub storage_table: &'a str,
    pub target_qualifier: &'a str,
    pub doc_id: DocId,
    pub document: &'a Document,
    pub checks: &'a [ViewCheckPlan],
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
}

pub fn validate_view_checks<S: Clone + 'static>(
    context: ViewCheckContext<'_, S>,
) -> Result<(), SQLError> {
    let ViewCheckContext {
        services,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
        checks,
        params,
        scope,
    } = context;
    if checks.is_empty() {
        return Ok(());
    }
    let row = super::rows::new_target_row_for_storage(
        services.rows,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
    )?;
    for check in checks {
        let value = eval_mutation_expr(
            services.expressions,
            scope,
            &check.predicate,
            Some(&row),
            params,
        )?;
        if !uqa_sql::expr::truthy(&value) {
            return Err(SQLError::Routine {
                sqlstate: "44000".into(),
                message: format!(
                    "new row violates check option for view \"{}\"",
                    RelationIdentity::from_legacy_name(&check.view)
                        .map_or_else(|_| check.view.clone(), |relation| relation.name)
                ),
            });
        }
    }
    Ok(())
}

pub fn refresh_stored_generated_columns<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    let columns = services
        .columns
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read generated columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    super::generated::refresh_stored_generated_columns(
        &columns,
        document,
        &mut |expression, row, schema| {
            crate::query::catalog_expression::eval_lowered_expression_with_schema(
                services.expressions.expressions,
                services.scopes.current_routine_scope(),
                expression,
                row,
                schema,
                &[],
            )
        },
    )
}

pub fn apply_missing_column_defaults<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let columns = services
        .columns
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("INSERT defaults", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    for definition in columns {
        let col = definition.name;
        if document.contains_key(&col) {
            continue;
        }
        if definition
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_identity)
        {
            continue;
        }
        if let Some(default_expr) = services
            .columns
            .try_column_insert_default_expr(table, &col)
            .map_err(|err| dml_storage_error("INSERT defaults", err))?
        {
            let value = uqa_sql::assignment::columns::coerce_to_column_type(
                services.assignment,
                services.columns,
                table,
                &col,
                crate::query::catalog_expression::eval_lowered_expression(
                    services.expressions.expressions,
                    services.scopes.current_routine_scope(),
                    &default_expr,
                    None,
                    params,
                )?,
            )?;
            document.insert(col, value);
        }
    }
    Ok(())
}
