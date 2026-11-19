//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, values};
use crate::{
    ast::{Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, PartitionStrategy},
    catalog::scalar_bindings::expression_type_with_work,
    ColumnType, RowSchema, SQLError,
};
use crate::{catalog::scalar_bindings::NativeScalarContext, expr::SQLValueCatalog};
use std::cmp::Ordering;
use uqa_core::{memory::ProductionControl, Value};

pub(super) async fn validate<'v>(
    spec: &PartitionSpec,
    types: &[ColumnType],
    bound: &PartitionBound,
    context: &NativeScalarContext<'_, '_>,
    values: &impl Fn() -> Option<&'v dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
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
                work.checkpoint().await?;
                let value = constant(entry, ty, context, values, control, work).await?;
                for previous in &entries[..position] {
                    work.checkpoint().await?;
                    if compare(
                        super::super::stored_datum(previous)?,
                        value,
                        ty,
                        values(),
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
            range(lower, types, context, values, control, work).await?;
            range(upper, types, context, values, control, work).await?;
            if super::super::comparison::range_points_with_work(
                lower, upper, types, values, control, work,
            )
            .await?
                != Ordering::Less
            {
                return Err(invalid("empty RANGE bound"));
            }
            Ok(())
        }
        _ => Err(invalid("bound strategy differs from partition key")),
    }
}

async fn constant<'a, 'v>(
    expression: &'a Expr,
    ty: &ColumnType,
    context: &NativeScalarContext<'_, '_>,
    values: &impl Fn() -> Option<&'v dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<&'a Value, SQLError> {
    control.check()?;
    let value = match expression {
        Expr::Literal(value) => value,
        Expr::TypedLiteral { value, .. } => {
            let declared = expression_type_with_work(
                expression,
                &RowSchema::default(),
                context,
                control,
                work,
            )
            .await?
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
    values::validate(value, ty, values, control, work).await?;
    Ok(value)
}

async fn range<'v>(
    point: &[PartitionRangeDatum],
    types: &[ColumnType],
    context: &NativeScalarContext<'_, '_>,
    values: &impl Fn() -> Option<&'v dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    if point.len() != types.len() {
        return Err(invalid("RANGE bound width"));
    }
    let mut infinite = None;
    for (datum, ty) in point.iter().zip(types) {
        work.checkpoint().await?;
        control.check()?;
        let direction = match datum {
            PartitionRangeDatum::MinValue => Some(false),
            PartitionRangeDatum::MaxValue => Some(true),
            PartitionRangeDatum::Value(expression) => {
                if matches!(
                    constant(expression, ty, context, values, control, work).await?,
                    Value::Null
                ) {
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

pub(super) async fn disjoint<'v>(
    left: &PartitionBound,
    right: &PartitionBound,
    types: &[ColumnType],
    values: &impl Fn() -> Option<&'v dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
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
                    work.checkpoint().await?;
                    if compare(
                        super::super::stored_datum(left)?,
                        super::super::stored_datum(right)?,
                        ty,
                        values(),
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
            super::super::comparison::range_points_with_work(
                left_lower,
                right_upper,
                types,
                values,
                control,
                work,
            )
            .await?
                == Ordering::Less
                && super::super::comparison::range_points_with_work(
                    right_lower,
                    left_upper,
                    types,
                    values,
                    control,
                    work,
                )
                .await?
                    == Ordering::Less
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
    values: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    crate::expr::compare_selected_values_with_catalog(left, right, Some(ty), control, values, None)
}
