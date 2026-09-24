//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array shape, NULL ordering and traversal stay here while callers supply element operators.

use super::{ArrayValue, Value};
use crate::{memory::ProductionControl, ValueRetentionError};
use std::cmp::Ordering;

impl ArrayValue {
    /// Compare row-major elements before dimensions and bounds. The callback only sees non-NULL elements reached before a decisive comparison.
    pub fn cmp_by_with_control<E: From<ValueRetentionError>>(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
        mut compare: impl FnMut(&Value, &Value, &ProductionControl<'_>) -> Result<Ordering, E>,
    ) -> Result<Ordering, E> {
        let mut left = self.elements_with_control(control)?;
        let mut right = other.elements_with_control(control)?;
        loop {
            let ordering = match (left.next_element()?, right.next_element()?) {
                (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
                (Some(Value::Null), Some(_)) | (Some(_), None) => Ordering::Greater,
                (Some(_), Some(Value::Null)) | (None, Some(_)) => Ordering::Less,
                (Some(left), Some(right)) => compare(left, right, control)?,
                (None, None) => break,
            };
            if !ordering.is_eq() {
                return Ok(ordering);
            }
        }
        Ok(self
            .dimensions()
            .len()
            .cmp(&other.dimensions().len())
            .then_with(|| self.dimensions().cmp(other.dimensions()))
            .then_with(|| self.lower_bounds().cmp(other.lower_bounds())))
    }

    /// Equality rejects different dimensions or bounds before invoking element equality, matching `PostgreSQL`'s array equality operator.
    pub fn eq_by_with_control<E: From<ValueRetentionError>>(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
        mut equal: impl FnMut(&Value, &Value, &ProductionControl<'_>) -> Result<bool, E>,
    ) -> Result<bool, E> {
        control.check()?;
        if self.dimensions() != other.dimensions() || self.lower_bounds() != other.lower_bounds() {
            return Ok(false);
        }
        let mut left = self.elements_with_control(control)?;
        let mut right = other.elements_with_control(control)?;
        loop {
            match (left.next_element()?, right.next_element()?) {
                (Some(Value::Null), Some(Value::Null)) => {}
                (Some(Value::Null), Some(_)) | (Some(_), Some(Value::Null)) => return Ok(false),
                (Some(left), Some(right)) if equal(left, right, control)? => {}
                (None, None) => return Ok(true),
                _ => return Ok(false),
            }
        }
    }
}
