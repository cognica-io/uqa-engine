//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate mutation defaults, typed assignments and view check options.
mod grouped;
mod subscripts;
pub use grouped::{
    eval_mutation_assignment_input, eval_typed_assignment_input, final_column_write,
    AssignmentInput, AssignmentSource, ViewRuleAssignment,
};

use super::{
    errors::dml_storage_error,
    expressions::eval_mutation_expr,
    rows::context::{MutationExpressionContext, MutationRowContext},
};
use crate::{
    query::{locking::context::RowLockScopeSource, CteScope},
    OwnedPhysicalRow,
};
use uqa_core::{DocId, Value};
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
    /// Whether the assignment builds a row an `INSERT` adds. `DEFAULT` leaves such a row's identity column out, for the identity generator to draw its value under the statement's `OVERRIDING` clause; it draws the next sequence value for a row an update assigns.
    pub new_row: bool,
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
        new_row,
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
        if let Some(sequence) =
            uqa_sql::assignment::columns::identity_column_sequence(services.columns, table, column)?
        {
            if new_row {
                return Ok(None);
            }
            return evaluate_column_default(
                services,
                table,
                column,
                Some(&uqa_sql::schema::sequences::implicit::sequence_next_value(
                    &sequence,
                )),
                params,
            )
            .map(Some);
        }
        let default = services
            .columns
            .try_column_insert_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?;
        return evaluate_column_default(services, table, column, default.as_ref(), params)
            .map(Some);
    }
    if generated.is_some() {
        return Err(if new_row {
            uqa_sql::semantics::generated_values::generated_column_insert_error(column)
        } else {
            uqa_sql::semantics::generated_values::generated_column_update_error(column)
        });
    }
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    let hook = services.expressions.expressions.bind_scope(ctes.clone());
    let source = uqa_sql::type_resolution::assignment_source_type(
        expression,
        schema,
        params,
        hook.as_ref(),
    )?;
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

/// Keep a default's declared source type through assignment: a domain value has
/// already passed its constraints when its expression returns it.
fn evaluate_column_default<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    table: &str,
    column: &str,
    expression: Option<&uqa_sql::ast::Expr>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let (value, source) = expression.map_or(Ok((Value::Null, None)), |expression| {
        crate::query::catalog_expression::eval_lowered_expression_with_type(
            services.expressions.expressions,
            services.scopes.current_routine_scope(),
            expression,
            None,
            params,
        )
    })?;
    uqa_sql::assignment::columns::coerce_to_column_type_from(
        services.assignment,
        services.columns,
        table,
        column,
        value,
        source.as_ref(),
    )
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
    let source = uqa_sql::type_resolution::assignment_source_type(
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
    input: ViewRuleAssignment<'_>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    for plan in &stmt.view_rule_update_plans {
        let Some(column) = plan.assigned_columns.get(input.position) else {
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
        return eval_typed_assignment_input(
            services,
            ctes,
            TypedAssignmentTarget {
                ty: schema.column_type(position),
                ..input.target
            },
            input.input,
            row,
            params,
        )
        .map(Some);
    }
    eval_typed_assignment_input(services, ctes, input.target, input.input, row, params).map(Some)
}

pub struct ViewCheckContext<'a, S: Clone + 'static> {
    pub services: MutationAssignmentContext<'a, S>,
    /// The catalog and authority that describe a row a check option rejects.
    pub constraints: super::constraints::ConstraintContext<'a>,
    /// The statement that writes the row, whose relation and columns the description follows.
    pub statement: super::constraints::ConstraintStatement<'a>,
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
        constraints,
        statement,
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
            return Err(super::constraints::view_check_violation(
                constraints,
                statement,
                &check.view,
                storage_table,
                document,
            ));
        }
    }
    Ok(())
}

pub fn refresh_stored_generated_columns<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    refresh_selected_stored_generated_columns(services, table, document, None)
}

/// Recompute only the requested generated columns for a schema rewrite, retaining unrelated stored values.
pub fn refresh_selected_stored_generated_columns<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    table: &str,
    document: &mut Document,
    selected: Option<&[String]>,
) -> Result<(), SQLError> {
    let columns = services
        .columns
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read generated columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    super::generated::refresh_stored_generated_columns(
        services.assignment,
        &columns,
        selected,
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
            let value =
                evaluate_column_default(services, table, &col, Some(&default_expr), params)?;
            document.insert(col, value);
        }
    }
    Ok(())
}
