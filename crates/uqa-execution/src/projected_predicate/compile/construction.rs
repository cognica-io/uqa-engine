//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional predicate constructors admit containers before moving child payload owners.

use super::super::{ProjectedExpr, ProjectedIntPredicate};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{ast::BinaryOp, expr::IntegerWidth, SQLError};

pub(super) fn inline(
    expression: ProjectedExpr,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    Ok(control.finish(expression, control.empty_reservation())?)
}

pub(super) fn literal(
    value: Produced<Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(ProjectedExpr::Literal(value), memory)?)
}

pub(super) fn boxed(
    expression: Produced<ProjectedExpr>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Box<ProjectedExpr>>, SQLError> {
    let header = control.reserve(size_of::<ProjectedExpr>())?;
    let (expression, memory) = expression.into_parts();
    Ok(control.finish(Box::new(expression), control.combine(memory, header))?)
}

pub(super) fn unary(
    expression: Produced<ProjectedExpr>,
    build: fn(Box<ProjectedExpr>) -> ProjectedExpr,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    let (expression, memory) = boxed(expression, control)?.into_parts();
    Ok(control.finish(build(expression), memory)?)
}

pub(super) fn boolean(
    items: Produced<Vec<ProjectedExpr>>,
    conjunction: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    if !conjunction {
        let (items, memory) = items.into_parts();
        return Ok(control.finish(ProjectedExpr::Or(items), memory)?);
    }
    if !items.iter().all(|item| {
        matches!(
            item,
            ProjectedExpr::IntFieldComparison { .. }
                | ProjectedExpr::IntFieldBetween { .. }
                | ProjectedExpr::IntFieldConjunction(_)
        )
    }) {
        let (items, memory) = items.into_parts();
        return Ok(control.finish(ProjectedExpr::And(items), memory)?);
    }
    let mut predicates = ProductionVec::new(*control);
    predicates.reserve(items.len())?;
    for item in items.iter() {
        control.check()?;
        match item {
            ProjectedExpr::IntFieldComparison {
                field,
                op,
                literal,
                field_on_left,
            } => push_integer(
                &mut predicates,
                ProjectedIntPredicate::Comparison {
                    field: *field,
                    op: *op,
                    literal: *literal,
                    field_on_left: *field_on_left,
                },
            )?,
            ProjectedExpr::IntFieldBetween { field, low, high } => push_integer(
                &mut predicates,
                ProjectedIntPredicate::Between {
                    field: *field,
                    low: *low,
                    high: *high,
                },
            )?,
            ProjectedExpr::IntFieldConjunction(items) => {
                for item in items {
                    push_integer(&mut predicates, *item)?;
                }
            }
            _ => unreachable!("integer predicate conjunction was validated"),
        }
    }
    let (predicates, memory) = predicates.finish()?.into_parts();
    Ok(control.finish(ProjectedExpr::IntFieldConjunction(predicates), memory)?)
}

fn push_integer(
    predicates: &mut ProductionVec<'_, ProjectedIntPredicate>,
    next: ProjectedIntPredicate,
) -> Result<(), SQLError> {
    if let Some(range) = predicates
        .last()
        .and_then(|previous| adjacent_range(previous, &next))
    {
        *predicates
            .as_mut_slice()
            .last_mut()
            .expect("preceding comparison") = range;
    } else {
        predicates.push_copy(next)?;
    }
    Ok(())
}

/// Borrowing one field is stable within the input row. Coalesce only adjacent
/// inclusive lower/upper comparisons; no cast, effect or intervening predicate moves.
/// Commuted operands keep their original order in fallback error diagnostics.
fn adjacent_range(
    lower: &ProjectedIntPredicate,
    upper: &ProjectedIntPredicate,
) -> Option<ProjectedIntPredicate> {
    use uqa_sql::ast::BinaryOp::{GreaterEqual, LessEqual};
    let (
        ProjectedIntPredicate::Comparison {
            field: lower_field,
            op: GreaterEqual,
            literal: low,
            field_on_left: true,
        },
        ProjectedIntPredicate::Comparison {
            field: upper_field,
            op: LessEqual,
            literal: high,
            field_on_left: true,
        },
    ) = (lower, upper)
    else {
        return None;
    };
    (*lower_field == *upper_field).then_some(ProjectedIntPredicate::Between {
        field: *lower_field,
        low: *low,
        high: *high,
    })
}

pub(super) fn binary(
    op: BinaryOp,
    lhs: Produced<ProjectedExpr>,
    rhs: Produced<ProjectedExpr>,
    integer_width: Option<IntegerWidth>,
    real_arithmetic: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        let selected = match (&*lhs, &*rhs) {
            (ProjectedExpr::Field(field), ProjectedExpr::Literal(Value::Int(literal))) => {
                Some((*field, *literal, true))
            }
            (ProjectedExpr::Literal(Value::Int(literal)), ProjectedExpr::Field(field)) => {
                Some((*field, *literal, false))
            }
            _ => None,
        };
        if let Some((field, literal, field_on_left)) = selected {
            return inline(
                ProjectedExpr::IntFieldComparison {
                    field,
                    op,
                    literal,
                    field_on_left,
                },
                control,
            );
        }
    }
    let lhs = boxed(lhs, control)?;
    let rhs = boxed(rhs, control)?;
    let (lhs, left_memory) = lhs.into_parts();
    let (rhs, right_memory) = rhs.into_parts();
    Ok(control.finish(
        ProjectedExpr::Binary {
            op,
            lhs,
            rhs,
            integer_width,
            real_arithmetic,
        },
        control.combine(left_memory, right_memory),
    )?)
}

pub(super) fn between(
    expression: Produced<ProjectedExpr>,
    low: Produced<ProjectedExpr>,
    high: Produced<ProjectedExpr>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    if let (
        ProjectedExpr::Field(field),
        ProjectedExpr::Literal(Value::Int(low)),
        ProjectedExpr::Literal(Value::Int(high)),
    ) = (&*expression, &*low, &*high)
    {
        return inline(
            ProjectedExpr::IntFieldBetween {
                field: *field,
                low: *low,
                high: *high,
            },
            control,
        );
    }
    let expression = boxed(expression, control)?;
    let low = boxed(low, control)?;
    let high = boxed(high, control)?;
    let (expression, memory) = expression.into_parts();
    let (low, low_memory) = low.into_parts();
    let (high, high_memory) = high.into_parts();
    Ok(control.finish(
        ProjectedExpr::Between {
            expression,
            low,
            high,
        },
        control.combine(memory, control.combine(low_memory, high_memory)),
    )?)
}

pub(super) fn membership(
    expression: Produced<ProjectedExpr>,
    list: Produced<Vec<ProjectedExpr>>,
    negated: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    let expression = boxed(expression, control)?;
    let (expression, memory) = expression.into_parts();
    let (list, list_memory) = list.into_parts();
    Ok(control.finish(
        ProjectedExpr::InList {
            expression,
            list,
            negated,
        },
        control.combine(memory, list_memory),
    )?)
}

pub(super) fn cast(
    expression: Produced<ProjectedExpr>,
    ty: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    let expression = boxed(expression, control)?;
    let ty = control.copy_text(ty)?;
    let (expression, memory) = expression.into_parts();
    let (ty, ty_memory) = ty.into_parts();
    Ok(control.finish(
        ProjectedExpr::Cast { expression, ty },
        control.combine(memory, ty_memory),
    )?)
}
