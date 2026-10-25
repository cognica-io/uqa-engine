//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural construction invokes the same selected support as external range input.

use super::{selected, RangeInputContext, SelectedInput};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{
    expr::{
        declared_multirange_datum_with_control, declared_range_datum_with_control,
        multirange_from_ranges_with_operations, RangeBoundsInput, RangeComparisonValue,
    },
    SQLError,
};

pub fn construct_range(
    context: &RangeInputContext<'_>,
    oid: u32,
    bounds: RangeBoundsInput,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let (definition, descriptor) = selected(context, oid)?;
    if oid != definition.oid {
        return Err(SQLError::Internal(
            "range constructor received a multirange type".into(),
        ));
    }
    let operations = SelectedInput {
        context,
        definition,
        descriptor: &descriptor,
    };
    let range = bounds.construct_with_operations(&operations, control)?;
    declared_range_datum_with_control(
        &range,
        &descriptor,
        context.expressions.composite_types(),
        control,
    )
}

pub fn construct_multirange(
    context: &RangeInputContext<'_>,
    oid: u32,
    members: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let (definition, descriptor) = selected(context, oid)?;
    if oid != definition.multirange.oid {
        return Err(SQLError::Internal(
            "multirange constructor received a range type".into(),
        ));
    }
    let operations = SelectedInput {
        context,
        definition,
        descriptor: &descriptor,
    };
    let mut ranges = ProductionVec::new(*control);
    ranges.reserve(members.len())?;
    for member in members {
        let range = match RangeComparisonValue::from_declared_with_control(
            member,
            definition.oid,
            context.expressions.enum_labels(),
            control,
        )? {
            Some(RangeComparisonValue::DeclaredRange(range)) => range,
            None => {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "multirange values cannot contain null members".into(),
                })
            }
            _ => {
                return Err(SQLError::Internal(
                    "multirange constructor received a non-range member".into(),
                ))
            }
        };
        ranges.push_produced(range)?;
    }
    let ranges = multirange_from_ranges_with_operations(ranges.finish()?, &operations, control)?;
    declared_multirange_datum_with_control(
        &ranges,
        &descriptor,
        context.expressions.composite_types(),
        control,
    )
}
