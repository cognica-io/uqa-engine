//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A SET group consumes one positional source before its targets apply their own coercions.

use super::{MutationAssignmentContext, MutationAssignmentTarget, TypedAssignmentTarget};
use crate::{query::CteScope, scalar::plan::PhysicalOuterRow, OwnedPhysicalRow};
use uqa_core::Value;
use uqa_sql::{
    ast::AssignmentTargets, expr::RowLookup, plan::AssignmentPlan, ColumnType, SQLError, SQLParam,
    ScalarExpr,
};

pub enum AssignmentSource<'a> {
    Expression(&'a ScalarExpr),
    Query { row: Option<OwnedPhysicalRow> },
}

pub enum AssignmentInput<'a> {
    Expression(&'a ScalarExpr),
    Value {
        value: Value,
        source: Option<&'a ColumnType>,
    },
}

impl<'a> AssignmentSource<'a> {
    pub fn evaluate<S: Clone + 'static>(
        services: MutationAssignmentContext<'_, S>,
        ctes: &CteScope<S>,
        assignment: &'a AssignmentPlan,
        row: Option<&OwnedPhysicalRow>,
        params: &[SQLParam],
    ) -> Result<Self, SQLError> {
        let AssignmentTargets::Multiple(targets) = &assignment.target else {
            return Ok(Self::Expression(&assignment.value));
        };
        let ScalarExpr::ScalarSubquery(index) = &assignment.value else {
            return Err(SQLError::Internal(
                "multiple-column assignment lost its query source".into(),
            ));
        };
        let query = ctes.scalar_subqueries.get(*index).ok_or_else(|| {
            SQLError::Internal("assignment subquery slot is outside its plan".into())
        })?;
        let hook = services.expressions.expressions.bind_scope(ctes.clone());
        let outer = row.map_or(PhysicalOuterRow::Absent, |row| PhysicalOuterRow::Physical {
            schema: &row.schema,
            row: &row.row,
        });
        let row = hook.row_subquery_value(*index, query, outer, params)?;
        if row
            .as_ref()
            .is_some_and(|row| row.schema.columns().len() != targets.source_width)
        {
            return Err(SQLError::Internal(
                "assignment query result changed its bound width".into(),
            ));
        }
        Ok(Self::Query { row })
    }

    pub fn input(&self, position: usize) -> AssignmentInput<'_> {
        match self {
            Self::Expression(expression) => AssignmentInput::Expression(expression),
            Self::Query { row } => AssignmentInput::Value {
                value: row
                    .as_ref()
                    .and_then(|row| row.positional_column(position))
                    .cloned()
                    .unwrap_or(Value::Null),
                source: row
                    .as_ref()
                    .and_then(|row| row.schema.column_type(position)),
            },
        }
    }
}

pub fn final_column_write(assignments: &[AssignmentPlan], group: usize, position: usize) -> bool {
    let targets = assignments[group].target.targets();
    !targets[position + 1..]
        .iter()
        .chain(
            assignments[group + 1..]
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
        )
        .any(|later| later.column == targets[position].column)
}

pub fn eval_mutation_assignment_input<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: MutationAssignmentTarget<'_>,
    input: AssignmentInput<'_>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    match input {
        AssignmentInput::Expression(expression) => {
            super::eval_mutation_assignment(services, ctes, target, expression, row, params)
        }
        AssignmentInput::Value { value, source } => {
            if uqa_sql::assignment::columns::generated_column_kind(
                services.columns,
                target.table,
                &target.target.column,
            )?
            .is_some()
            {
                return Err(
                    uqa_sql::semantics::generated_values::generated_column_update_error(
                        &target.target.column,
                    ),
                );
            }
            super::coerce_mutation_assignment(services, ctes, target, value, source, row, params)
                .map(Some)
        }
    }
}

pub fn eval_typed_assignment_input<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    target: TypedAssignmentTarget<'_>,
    input: AssignmentInput<'_>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    match input {
        AssignmentInput::Expression(expression) => {
            super::eval_typed_assignment(services, ctes, target, expression, row, params)
        }
        AssignmentInput::Value { value, source } => {
            super::coerce_typed_assignment(services, ctes, target, value, source, row, params)
        }
    }
}

pub struct ViewRuleAssignment<'a> {
    pub position: usize,
    pub target: TypedAssignmentTarget<'a>,
    pub input: AssignmentInput<'a>,
}
