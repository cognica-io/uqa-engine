//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare pairs of ROW fields in SQL evaluation order.

use super::{
    collations, compare_declared_range, compare_nullable_with_catalog,
    compare_typed_values_with_catalog, equal_values, values_equal_nullable_with_catalog,
    EnumComparisonState, EnumLabelCatalog, Ordering, ProductionControl, Result, Value,
};
use crate::ast::ComparisonOperator;
use crate::expr::{EngineHook, EngineValueCatalog, SQLValueCatalog};
use std::ops::Deref;
use uqa_core::{memory::Produced, RowValue};

/// Compare two uncast ROW constructors. Each reached pair evaluates its left field then its right field. Nested row-valued fields use record operators.
pub fn eval_row_comparison_with_control<T>(
    op: ComparisonOperator,
    left: &[T],
    right: &[T],
    control: &ProductionControl<'_>,
    engine: Option<&dyn EngineHook>,
    state: Option<&EnumComparisonState>,
    mut evaluate: impl FnMut(&T) -> Result<(Produced<Value>, Option<u32>)>,
) -> Result<Option<bool>> {
    control.check()?;
    crate::type_resolution::row_comparison::validate_width(left.len(), right.len(), false)?;
    let catalog = engine.map(EngineValueCatalog);
    let catalog = catalog
        .as_ref()
        .map(|catalog| catalog as &dyn SQLValueCatalog);
    let pairs = left.iter().zip(right).map(|(left, right)| {
        let (left, left_oid) = evaluate(left)?;
        let (right, right_oid) = evaluate(right)?;
        Ok((left, right, left_oid.or(right_oid)))
    });
    match op {
        ComparisonOperator::Equal | ComparisonOperator::NotEqual => {
            Ok(equal_fields(pairs, control, catalog, engine)?.map(|equal| {
                if op == ComparisonOperator::Equal {
                    equal
                } else {
                    !equal
                }
            }))
        }
        _ => {
            let order = compare_fields(pairs, Ordering::Equal, control, catalog, state, engine)?;
            super::apply_order(op.binary(), order)
        }
    }
}

pub(super) fn value_pairs<'a>(
    left: &'a RowValue,
    right: &'a RowValue,
) -> impl Iterator<Item = Result<(&'a Value, &'a Value, Option<u32>)>> {
    left.iter()
        .zip(right)
        .enumerate()
        .map(|(index, (lhs, rhs))| {
            let field = |row: &RowValue| row.field_types()?.get(index).map(|field| field.oid);
            Ok((lhs, rhs, field(left).or_else(|| field(right))))
        })
}

pub(super) fn equal_fields<V: Deref<Target = Value>>(
    pairs: impl Iterator<Item = Result<(V, V, Option<u32>)>>,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
    engine: Option<&dyn EngineHook>,
) -> Result<Option<bool>> {
    let mut unknown = false;
    for pair in pairs {
        let (lhs, rhs, oid) = pair?;
        let (left, right) = (&*lhs, &*rhs);
        let equal = if let Some(order) = compare_declared_range(left, right, oid, control, enums)? {
            Some(order.is_eq())
        } else if matches!((left, right), (Value::Row(_), Value::Row(_))) {
            Some(equal_values(
                left,
                right,
                control,
                enums,
                false,
                collations::DEFAULT,
            )?)
        } else {
            values_equal_nullable_with_catalog(left, right, control, enums, engine)?
        };
        match equal {
            Some(false) => return Ok(Some(false)),
            Some(true) => {}
            None => unknown = true,
        }
    }
    Ok((!unknown).then_some(true))
}

pub(super) fn compare_fields<V: Deref<Target = Value>>(
    pairs: impl Iterator<Item = Result<(V, V, Option<u32>)>>,
    when_equal: Ordering,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
    state: Option<&EnumComparisonState>,
    engine: Option<&dyn EngineHook>,
) -> Result<Option<Ordering>> {
    for (index, pair) in pairs.enumerate() {
        let (lhs, rhs, oid) = pair?;
        let (left, right) = (&*lhs, &*rhs);
        let order = if let Some(order) = compare_declared_range(left, right, oid, control, enums)? {
            Some(order)
        } else if matches!((left, right), (Value::Row(_), Value::Row(_))) {
            Some(compare_typed_values_with_catalog(
                left, right, control, enums,
            )?)
        } else {
            let field = if matches!(left, Value::Enum(_) | Value::Datum(_))
                && !matches!(right, Value::Null)
            {
                state.map(|state| state.field(index))
            } else {
                None
            };
            compare_nullable_with_catalog(left, right, control, enums, field.as_deref(), engine)?
        };
        if order != Some(Ordering::Equal) {
            return Ok(order);
        }
    }
    Ok(Some(when_equal))
}
