//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array-element ordering with fallible composite comparisons.

use super::{
    copy_elements, ArrayValue, Produced, ProductionControl, ProductionVec, Result, SQLError, Value,
};
use std::cmp::Ordering;
use uqa_core::ordering::sort_by_with_control;

pub(super) fn sorted_elements(
    array: &ArrayValue,
    descending: bool,
    nulls_first: bool,
    declared_json: bool,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
) -> Result<Produced<Vec<Value>>> {
    if array.elements().len() > 1 {
        crate::expr::binary::require_array_element_support(
            array,
            catalog,
            control,
            crate::expr::binary::ComparisonSupport::Ordering,
            if array.dimensions().len() <= 1 {
                "0A000"
            } else {
                "42883"
            },
        )?;
    }
    if array.elements().len() > 1
        && array.dimensions().len() <= 1
        && (declared_json
            || array
                .elements()
                .iter()
                .any(|element| matches!(element, Value::Json(_))))
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "could not identify a comparison function for type json".into(),
        });
    }
    let mut order = ProductionVec::new(*control);
    for (index, value) in array.elements().iter().enumerate() {
        order.push_copy((index, value))?;
    }
    let mut order = order.finish()?;
    sort_by_with_control(
        order.as_mut_slice(),
        &mut || control.check().map_err(SQLError::from),
        |(left_index, left), (right_index, right), _| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) if nulls_first => Ordering::Less,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) if nulls_first => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                (left, right) => {
                    let order = crate::expr::binary::compare_typed_values_with_enum_catalog(
                        left, right, control, catalog, None,
                    )?;
                    if descending {
                        order.reverse()
                    } else {
                        order
                    }
                }
            };
            // Match the original stable sort without allocating a second owned array or sort buffer.
            Ok(ordering.then_with(|| left_index.cmp(right_index)))
        },
    )?;
    copy_elements(order.iter().map(|(_, value)| *value), control)
}
