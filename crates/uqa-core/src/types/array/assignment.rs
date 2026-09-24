//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded array replacement preserves `PostgreSQL` dimensions and row-major slice input.

use super::{ArrayValue, ControlledArrayElements};
use crate::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value, ValueRetentionError,
};

const MAX_DIMENSIONS: usize = 6;
const MAX_ELEMENTS: usize = 134_217_727;

#[derive(Debug, thiserror::Error)]
pub enum ArrayAssignmentError {
    #[error("wrong number of array subscripts")]
    SubscriptCount,
    #[error("array subscript out of range")]
    SubscriptRange,
    #[error("upper bound cannot be less than lower bound")]
    ReversedBounds,
    #[error("array slice subscript must provide both boundaries")]
    MissingBounds,
    #[error("source array too small")]
    SourceTooSmall,
    #[error("array size exceeds the maximum allowed (134217727)")]
    SizeLimit,
    #[error("array lower bound is too large: {0}")]
    LowerBound(i32),
    #[error("invalid array assignment element shape")]
    ElementShape,
    #[error(transparent)]
    Retention(#[from] ValueRetentionError),
}

type Result<T> = std::result::Result<T, ArrayAssignmentError>;

#[derive(Clone, Copy, Default)]
struct Range {
    lower: i32,
    length: usize,
}

impl Range {
    fn new(lower: i32, upper: i32) -> Result<Self> {
        let length = usize::try_from(i64::from(upper) - i64::from(lower) + 1)
            .map_err(|_| ArrayAssignmentError::SizeLimit)?;
        if i32::try_from(length).is_err() {
            return Err(ArrayAssignmentError::SizeLimit);
        }
        Ok(Self { lower, length })
    }

    fn upper(self) -> i64 {
        i64::from(self.lower) + self.length as i64 - 1
    }

    fn contains(self, position: i64) -> bool {
        i64::from(self.lower) <= position && position <= self.upper()
    }
}

struct Layout {
    output: [Range; MAX_DIMENSIONS],
    selected: [Range; MAX_DIMENSIONS],
    dimensions: usize,
}

impl Layout {
    fn new(dimensions: usize) -> Self {
        Self {
            output: [Range::default(); MAX_DIMENSIONS],
            selected: [Range::default(); MAX_DIMENSIONS],
            dimensions,
        }
    }

    fn check(&self) -> Result<usize> {
        let count = item_count(&self.output[..self.dimensions])?;
        for range in &self.output[..self.dimensions] {
            if i64::from(range.lower) + range.length as i64 > i64::from(i32::MAX) {
                return Err(ArrayAssignmentError::LowerBound(range.lower));
            }
        }
        Ok(count)
    }
}

fn item_count(ranges: &[Range]) -> Result<usize> {
    let mut count = usize::from(!ranges.is_empty());
    for range in ranges {
        count = count
            .checked_mul(range.length)
            .filter(|&count| i32::try_from(count).is_ok())
            .ok_or(ArrayAssignmentError::SizeLimit)?;
    }
    if count > MAX_ELEMENTS {
        return Err(ArrayAssignmentError::SizeLimit);
    }
    Ok(count)
}

impl ArrayValue {
    /// Assign one scalar element. Empty arrays acquire the supplied bounds; only one-dimensional nonempty arrays may grow. SQL owns index coercion and element typing before this call.
    pub fn assign_element_with_control(
        &self,
        indices: &[i32],
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        control.check()?;
        let dimensions = self.assignment_dimensions();
        if indices.is_empty()
            || indices.len() > MAX_DIMENSIONS
            || (dimensions != 0 && dimensions != indices.len())
        {
            return Err(ArrayAssignmentError::SubscriptCount);
        }
        let mut layout = Layout::new(indices.len());
        for (dimension, &index) in indices.iter().enumerate() {
            let selected = Range::new(index, index)?;
            layout.selected[dimension] = selected;
            layout.output[dimension] = self.assignment_range(dimension, dimensions, selected)?;
        }
        if matches!(value, Value::List(_) | Value::Array(_)) {
            return Err(ArrayAssignmentError::ElementShape);
        }
        self.replace(&layout, Replacement::Element(value), control)
    }

    /// Assign a non-NULL slice from its row-major element stream, ignoring excess source elements and preserving destination bounds. Omitted bounds use the existing dimension; empty destinations require explicit bounds. SQL handles a NULL source as a no-op before calling this method.
    pub fn assign_slice_with_control(
        &self,
        bounds: &[(Option<i32>, Option<i32>)],
        source: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        control.check()?;
        let dimensions = self.assignment_dimensions();
        if bounds.is_empty()
            || bounds.len() > MAX_DIMENSIONS
            || dimensions > MAX_DIMENSIONS
            || (dimensions != 0 && bounds.len() > dimensions)
        {
            return Err(ArrayAssignmentError::SubscriptCount);
        }
        let mut layout = Layout::new(dimensions.max(bounds.len()));
        for dimension in 0..layout.dimensions {
            let original = (dimensions != 0).then(|| self.range(dimension)).flatten();
            let (lower, upper) = bounds.get(dimension).copied().unwrap_or((None, None));
            let lower = lower.or_else(|| original.map(|range| range.lower));
            let upper = upper.or_else(|| original.and_then(|range| range.upper().try_into().ok()));
            let (Some(lower), Some(upper)) = (lower, upper) else {
                return Err(ArrayAssignmentError::MissingBounds);
            };
            if dimensions != 0 && lower > upper {
                return Err(ArrayAssignmentError::ReversedBounds);
            }
            let selected = Range::new(lower, upper)?;
            layout.selected[dimension] = selected;
            layout.output[dimension] = self.assignment_range(dimension, dimensions, selected)?;
        }
        if dimensions != 0 {
            layout.check()?;
        }
        let needed = item_count(&layout.selected[..layout.dimensions])?;
        let available = source.dimensions().iter().try_fold(
            usize::from(!source.dimensions().is_empty()),
            |count, length| count.checked_mul(*length),
        );
        if available.is_none_or(|available| needed > available) {
            return Err(ArrayAssignmentError::SourceTooSmall);
        }
        self.replace(
            &layout,
            Replacement::Slice(source.elements_with_control(control)?),
            control,
        )
    }

    fn assignment_dimensions(&self) -> usize {
        if self.dimensions().contains(&0) {
            0
        } else {
            self.dimensions().len()
        }
    }

    fn range(&self, dimension: usize) -> Option<Range> {
        Some(Range {
            lower: self.lower_bound(dimension)?,
            length: *self.dimensions().get(dimension)?,
        })
    }

    fn assignment_range(
        &self,
        dimension: usize,
        dimensions: usize,
        selected: Range,
    ) -> Result<Range> {
        if dimensions == 0 {
            return Ok(selected);
        }
        let original = self
            .range(dimension)
            .expect("validated assignment dimension");
        if dimensions == 1 {
            let upper = original.upper().max(selected.upper());
            return Range::new(
                original.lower.min(selected.lower),
                upper
                    .try_into()
                    .map_err(|_| ArrayAssignmentError::SizeLimit)?,
            );
        }
        if !original.contains(i64::from(selected.lower)) || !original.contains(selected.upper()) {
            return Err(ArrayAssignmentError::SubscriptRange);
        }
        Ok(original)
    }

    fn replace(
        &self,
        layout: &Layout,
        mut replacement: Replacement<'_, '_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        let count = layout.check()?;
        let mut bounds = ProductionVec::new(*control);
        let elements = if count == 0 {
            ProductionVec::new(*control).finish()?
        } else {
            for range in &layout.output[..layout.dimensions] {
                bounds.push_copy(range.lower)?;
            }
            rebuild(
                self,
                (self.assignment_dimensions() != 0).then(|| self.elements()),
                layout,
                0,
                &mut replacement,
                control,
            )?
        };
        Self::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
            .ok_or(ArrayAssignmentError::ElementShape)
    }
}

enum Replacement<'v, 'c> {
    Element(&'v Value),
    Slice(ControlledArrayElements<'v, 'c>),
}

impl Replacement<'_, '_> {
    fn next(&mut self, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
        let value = match self {
            Self::Element(value) => *value,
            Self::Slice(cursor) => cursor
                .next_element()?
                .ok_or(ArrayAssignmentError::SourceTooSmall)?,
        };
        Ok(control.copy_value(value)?)
    }
}

fn rebuild(
    original: &ArrayValue,
    elements: Option<&[Value]>,
    layout: &Layout,
    dimension: usize,
    replacement: &mut Replacement<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let range = layout.output[dimension];
    let mut output = ProductionVec::new(*control);
    output.reserve(range.length)?;
    for offset in 0..range.length {
        let position = i64::from(range.lower) + offset as i64;
        let selected = layout.selected[dimension].contains(position);
        let existing = original.lower_bound(dimension).and_then(|lower| {
            let index = usize::try_from(position - i64::from(lower)).ok()?;
            elements?.get(index)
        });
        let value = if !selected {
            control.copy_value(existing.unwrap_or(&Value::Null))?
        } else if dimension + 1 == layout.dimensions {
            replacement.next(control)?
        } else {
            let children = match existing {
                Some(Value::List(values)) => Some(values.as_slice()),
                None => None,
                _ => return Err(ArrayAssignmentError::ElementShape),
            };
            let (values, memory) = rebuild(
                original,
                children,
                layout,
                dimension + 1,
                replacement,
                control,
            )?
            .into_parts();
            control.finish(Value::List(values), memory)?
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests;
