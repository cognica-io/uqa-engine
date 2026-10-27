//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host constructors reuse the selected native range producer and admitted value readers.

use super::{RangeBoundsInput, RangeComparisonValue, RangeSemantics, RangeSubtype};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub fn range_value_from_bounds_with_control(
    subtype: RangeSubtype,
    bounds: RangeBoundsInput,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let range = bounds.construct_with_operations(RangeSemantics::native(subtype), control)?;
    let (text, memory) = range.to_text_with_control(control)?.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

pub fn multirange_value_from_members_with_control(
    subtype: RangeSubtype,
    members: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut ranges = ProductionVec::new(*control);
    ranges.reserve(members.len())?;
    for member in members {
        let Some(RangeComparisonValue::Range(range)) =
            RangeComparisonValue::from_type_with_control(
                member,
                &ColumnType::Range(subtype),
                None,
                control,
            )?
        else {
            return Err(null_multirange_member());
        };
        ranges.push_produced(range)?;
    }
    let ranges = super::multirange_from_ranges_with_operations(
        ranges.finish()?,
        RangeSemantics::native(subtype),
        control,
    )?;
    let (text, memory) = ranges.to_text_with_control(control)?.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

pub(super) fn null_multirange_member() -> SQLError {
    SQLError::Routine {
        sqlstate: "22004".into(),
        message: "multirange values cannot contain null members".into(),
    }
}
