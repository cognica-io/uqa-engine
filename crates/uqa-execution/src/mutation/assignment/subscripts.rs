//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose partial writes while every bound and RHS sees the unchanged input row.

use super::{MutationAssignmentContext, MutationAssignmentTarget, TypedAssignmentTarget};
use crate::{mutation::expressions::eval_mutation_expr, query::CteScope, OwnedPhysicalRow};
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::assignment::containers::assign_array_with_control;
use uqa_sql::{
    assignment::{columns::coerce_column_value, targets},
    ast::AssignmentStep,
    ColumnType, SQLError, SQLParam, ScalarExpr,
};

pub(super) fn assign_value<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    destination: MutationAssignmentTarget<'_>,
    value: impl FnOnce() -> Result<Value, SQLError>,
    source: Option<&ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let MutationAssignmentTarget {
        table,
        target,
        current,
        final_column_write,
        ..
    } = destination;
    if target.is_whole_column() {
        return uqa_sql::assignment::columns::coerce_to_column_type_from(
            services.assignment,
            services.columns,
            table,
            &target.column,
            value()?,
            source,
        );
    }
    let columns = services
        .columns
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read assignment type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let declared = &columns
        .iter()
        .find(|column| column.name == target.column)
        .ok_or_else(|| SQLError::UnknownColumn(target.column.clone()))?
        .ty;
    assign_typed_value(
        services,
        ctes,
        TypedAssignmentTarget {
            target,
            ty: Some(declared),
            current,
            final_column_write,
        },
        value,
        source,
        row,
        params,
    )
}

/// View and table assignments share the same typed partial-write semantics.
pub(super) fn assign_typed_value<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    destination: TypedAssignmentTarget<'_>,
    value: impl FnOnce() -> Result<Value, SQLError>,
    source: Option<&ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let TypedAssignmentTarget {
        target,
        ty,
        current,
        final_column_write,
    } = destination;
    let declared = ty.expect("typed assignment has its declared destination");
    if target.is_whole_column() {
        return coerce_column_value(services.assignment, value()?, declared, source);
    }
    let bound = |expr: &ScalarExpr| -> Result<i32, SQLError> {
        let value = eval_mutation_expr(services.expressions, ctes, expr, row, params)?;
        if value == Value::Null {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: "array subscript in assignment must not be null".into(),
            });
        }
        let value = coerce_column_value(services.assignment, value, &ColumnType::Integer, None)?;
        match value {
            Value::Int(value) => i32::try_from(value).map_err(|_| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "integer out of range".into(),
            }),
            _ => Err(SQLError::Internal(
                "integer assignment conversion returned a noninteger".into(),
            )),
        }
    };
    if targets::has_field_step(target) {
        return assign_field_value(
            services.assignment,
            FieldAssignment {
                target,
                declared,
                current: current.unwrap_or(&Value::Null),
                final_column_write,
            },
            value,
            source,
            &bound,
        );
    }
    let required = targets::assignment_value_type(target, declared)?;
    targets::validate_assignment_source(target, &required, source)?;
    targets::validate_assignment_result(target, declared)?;
    let slice = target
        .indirection
        .iter()
        .any(|step| matches!(step, AssignmentStep::Slice { .. }));
    let bounds = assignment_bounds(&target.indirection, &bound)?;
    let value = coerce_column_value(services.assignment, value()?, &required, source)?;
    let result = assign_array_with_control(
        current.unwrap_or(&Value::Null),
        &value,
        &bounds,
        slice,
        Some(&uqa_sql::expr::EngineValueCatalog(services.assignment)),
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary array assignment result");
    if final_column_write {
        coerce_column_value(
            services.assignment,
            result,
            declared,
            Some(&targets::array_assignment_type(target, declared)?),
        )
    } else {
        Ok(result)
    }
}

struct FieldAssignment<'a> {
    target: &'a uqa_sql::ast::AssignmentTarget<ScalarExpr>,
    declared: &'a ColumnType,
    current: &'a Value,
    final_column_write: bool,
}

/// A write through composite fields and subscript groups, as `transformAssignmentIndirection` nests `FieldStore` and subscripting: each level rebuilds its container around the level below, and a NULL composite becomes a row of NULL fields. Subscript bounds are evaluated in the target's order before the value.
fn assign_field_value(
    assignment: &dyn uqa_sql::assignment::AssignmentContext,
    destination: FieldAssignment<'_>,
    value: impl FnOnce() -> Result<Value, SQLError>,
    source: Option<&ColumnType>,
    bound: &dyn Fn(&ScalarExpr) -> Result<i32, SQLError>,
) -> Result<Value, SQLError> {
    let FieldAssignment {
        target,
        declared,
        current,
        final_column_write,
    } = destination;
    let composites = uqa_sql::expr::EngineHook::composite_types(assignment);
    let types = targets::field_assignment_types(target, declared, composites)?;
    let required = types
        .last()
        .ok_or_else(|| SQLError::Internal("field assignment has no value type".into()))?;
    targets::validate_assignment_source(target, required, source)?;
    let levels = targets::assignment_levels(&target.indirection);
    let mut level_bounds = Vec::with_capacity(levels.len());
    for level in &levels {
        let targets::AssignmentLevel::Subscripts(steps) = level else {
            level_bounds.push(Vec::new());
            continue;
        };
        level_bounds.push(assignment_bounds(steps, bound)?);
    }
    let value = coerce_column_value(assignment, value()?, required, source)?;
    let binding = uqa_sql::assignment::path::bind(target, &types, composites)?;
    let path = binding
        .path
        .iter()
        .zip(&level_bounds)
        .map(|(step, bounds)| {
            use uqa_sql::{assignment::path::AssignmentContainer, ast::AssignmentPathStep};
            Ok(match step {
                AssignmentPathStep::Field { type_oid, number } => AssignmentContainer::Field {
                    type_oid: *type_oid,
                    number: *number,
                },
                AssignmentPathStep::Subscripts { slice, .. } => {
                    AssignmentContainer::subscripts(bounds, *slice)?
                }
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let result = uqa_sql::assignment::path::assign_with_control(
        current,
        &path,
        &value,
        Some(&uqa_sql::expr::EngineValueCatalog(assignment)),
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary field assignment result");
    if !final_column_write {
        return Ok(result);
    }
    // A domain over the composite checks the rebuilt value.
    let mut base = declared;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    coerce_column_value(assignment, result, declared, Some(base))
}

fn assignment_bounds(
    steps: &[AssignmentStep<ScalarExpr>],
    bound: &dyn Fn(&ScalarExpr) -> Result<i32, SQLError>,
) -> Result<Vec<uqa_sql::assignment::containers::SubscriptBound>, SQLError> {
    let slice = steps
        .iter()
        .any(|step| matches!(step, AssignmentStep::Slice { .. }));
    Ok(uqa_sql::assignment::containers::evaluate_subscript_bounds(
        steps.len(),
        slice,
        &ProductionControl::uncontrolled(),
        |dimension, upper_bound| match &steps[dimension] {
            AssignmentStep::Index(index) => {
                if upper_bound {
                    bound(index).map(Some)
                } else {
                    Ok(Some(1))
                }
            }
            AssignmentStep::Slice { lower, upper } => if upper_bound { upper } else { lower }
                .as_deref()
                .map(bound)
                .transpose(),
            AssignmentStep::Field(_) => unreachable!("subscript group"),
        },
    )?
    .into_uncontrolled()
    .expect("ordinary assignment bounds"))
}
