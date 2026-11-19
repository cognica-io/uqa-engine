//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, values, NativePartitionContext};
use crate::{
    ast::{Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy},
    catalog::scalar_bindings::expression_type,
    ColumnType, RowSchema, SQLError,
};
use std::cmp::Ordering;
use uqa_core::{memory::ProductionControl, Value};

pub(super) fn validate(
    spec: &PartitionSpec,
    types: &[ColumnType],
    bound: &PartitionBound,
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match (spec.strategy, bound) {
        (PartitionStrategy::List | PartitionStrategy::Range, PartitionBound::Default) => Ok(()),
        (PartitionStrategy::Hash, PartitionBound::Hash { modulus, remainder }) => {
            super::super::hash::validate_bound(*modulus, *remainder)
        }
        (PartitionStrategy::List, PartitionBound::List(entries)) => {
            let [ty] = types else {
                return Err(invalid("LIST key width"));
            };
            if entries.is_empty() {
                return Err(invalid("empty LIST bound"));
            }
            for (position, entry) in entries.iter().enumerate() {
                let value = constant(entry, ty, context, control)?;
                for previous in &entries[..position] {
                    if compare(
                        super::super::stored_datum(previous)?,
                        value,
                        ty,
                        context,
                        control,
                    )?
                    .is_eq()
                    {
                        return Err(invalid("duplicate LIST datum"));
                    }
                }
            }
            Ok(())
        }
        (PartitionStrategy::Range, PartitionBound::Range { lower, upper }) => {
            range(lower, types, context, control)?;
            range(upper, types, context, control)?;
            if range_order(lower, upper, types, context, control)? != Ordering::Less {
                return Err(invalid("empty RANGE bound"));
            }
            Ok(())
        }
        _ => Err(invalid("bound strategy differs from partition key")),
    }
}

fn constant<'a>(
    expression: &'a Expr,
    ty: &ColumnType,
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<&'a Value, SQLError> {
    control.check()?;
    let value = match expression {
        Expr::Literal(value) => value,
        Expr::TypedLiteral { value, .. } => {
            let declared =
                expression_type(expression, &RowSchema::default(), &context.scalar, control)?
                    .ok_or_else(|| invalid("unresolved constant annotation"))?;
            if crate::catalog::type_metadata::pg_type_oid(&declared)
                != crate::catalog::type_metadata::pg_type_oid(ty)
            {
                return Err(invalid("constant type annotation"));
            }
            value
        }
        _ => return Err(invalid("bound is not a stored constant")),
    };
    values::validate(value, ty, context.values, control)?;
    Ok(value)
}

fn range(
    point: &[PartitionRangeDatum],
    types: &[ColumnType],
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if point.len() != types.len() {
        return Err(invalid("RANGE bound width"));
    }
    let mut infinite = None;
    for (datum, ty) in point.iter().zip(types) {
        control.check()?;
        let direction = match datum {
            PartitionRangeDatum::MinValue => Some(false),
            PartitionRangeDatum::MaxValue => Some(true),
            PartitionRangeDatum::Value(expression) => {
                if matches!(constant(expression, ty, context, control)?, Value::Null) {
                    return Err(invalid("NULL in RANGE bound"));
                }
                None
            }
        };
        if infinite.is_some() && infinite != direction {
            return Err(invalid("inconsistent infinite RANGE suffix"));
        }
        infinite = direction;
    }
    Ok(())
}

pub(super) fn disjoint(
    left: &PartitionBound,
    right: &PartitionBound,
    types: &[ColumnType],
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let overlaps = match (left, right) {
        (PartitionBound::Default, PartitionBound::Default) => true,
        (PartitionBound::Default, _) | (_, PartitionBound::Default) => false,
        (PartitionBound::List(left), PartitionBound::List(right)) => {
            let [ty] = types else {
                return Err(invalid("LIST key width"));
            };
            for left in left {
                for right in right {
                    if compare(
                        super::super::stored_datum(left)?,
                        super::super::stored_datum(right)?,
                        ty,
                        context,
                        control,
                    )?
                    .is_eq()
                    {
                        return Err(invalid("overlapping LIST siblings"));
                    }
                }
            }
            false
        }
        (
            PartitionBound::Range {
                lower: left_lower,
                upper: left_upper,
            },
            PartitionBound::Range {
                lower: right_lower,
                upper: right_upper,
            },
        ) => {
            range_order(left_lower, right_upper, types, context, control)? == Ordering::Less
                && range_order(right_lower, left_upper, types, context, control)? == Ordering::Less
        }
        (
            PartitionBound::Hash {
                modulus: left,
                remainder: left_remainder,
            },
            PartitionBound::Hash {
                modulus: right,
                remainder: right_remainder,
            },
        ) => {
            if left.max(right) % left.min(right) != 0 {
                return Err(invalid("HASH moduli do not form a divisibility chain"));
            }
            super::super::comparison::first_shared_hash_residue(
                *left,
                *left_remainder,
                *right,
                *right_remainder,
                *left.max(right),
            )
            .is_some()
        }
        _ => return Err(invalid("sibling strategy mismatch")),
    };
    if overlaps {
        Err(invalid("overlapping partition siblings"))
    } else {
        Ok(())
    }
}

fn compare(
    left: &Value,
    right: &Value,
    ty: &ColumnType,
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    crate::expr::compare_selected_values_with_catalog(
        left,
        right,
        Some(ty),
        control,
        context.values,
        None,
    )
}

fn range_order(
    left: &[PartitionRangeDatum],
    right: &[PartitionRangeDatum],
    types: &[ColumnType],
    context: &NativePartitionContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    super::super::comparison::range_points(left, right, types, context.values, control)
}
