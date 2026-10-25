//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural observation borrows admitted bounds and encodes members without construction effects.

use super::{CanonicalRange, RangeComparisonValue};
use crate::{expr::SQLValueCatalog, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub struct RangeBoundValues<'a> {
    pub lower: Option<&'a Value>,
    pub upper: Option<&'a Value>,
    pub lower_inclusive: bool,
    pub upper_inclusive: bool,
    pub empty: bool,
}

impl<'a> RangeBoundValues<'a> {
    fn from_range<I>(range: &'a CanonicalRange<I>) -> Self {
        Self {
            lower: range.lower(),
            upper: range.upper(),
            lower_inclusive: range.lower_inclusive(),
            upper_inclusive: range.upper_inclusive(),
            empty: range.is_empty(),
        }
    }
}

impl RangeComparisonValue {
    #[must_use]
    pub fn bounds(&self) -> Option<RangeBoundValues<'_>> {
        match self {
            Self::Range(range) => Some(RangeBoundValues::from_range(range)),
            Self::DeclaredRange(range) => Some(RangeBoundValues::from_range(range)),
            _ => None,
        }
    }

    #[must_use]
    pub fn member_count(&self) -> Option<usize> {
        match self {
            Self::Multirange(ranges) => Some(ranges.ranges().len()),
            Self::DeclaredMultirange(ranges) => Some(ranges.ranges().len()),
            _ => None,
        }
    }

    /// Copy one normalized member through its native carrier encoder. No type input, comparison or canonical function executes.
    pub fn member_value_with_control(
        &self,
        index: usize,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Value>>, SQLError> {
        control.check()?;
        match self {
            Self::Multirange(ranges) => {
                let Some(range) = ranges.ranges().get(index) else {
                    return Ok(None);
                };
                let (text, memory) = range.to_text_with_control(control)?.into_parts();
                Ok(Some(control.finish(Value::Str(text), memory)?))
            }
            Self::DeclaredMultirange(ranges) => {
                let Some(range) = ranges.ranges().get(index) else {
                    return Ok(None);
                };
                let descriptor = catalog
                    .map(|catalog| catalog.value_range_type(*range.identity()))
                    .transpose()?
                    .flatten()
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: format!("type with OID {} does not exist", range.identity()),
                    })?;
                let adapter = crate::expr::value_catalog::ValueCompositeCatalog(catalog);
                Ok(Some(super::declared_range_datum_with_control(
                    range,
                    &descriptor,
                    Some(&adapter),
                    control,
                )?))
            }
            _ => Ok(None),
        }
    }
}
