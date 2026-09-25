//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;
use crate::vector_index::validate_vector_values_controlled;
use crate::{StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactVectorReason {
    ZeroNorm,
    NonFiniteNorm,
}

/// Classification applies equally to indexed vectors and queries. Exact inputs retain their raw canonical scoring path.
#[derive(Debug)]
pub enum NavigationInput {
    Navigable(NavigationVector),
    Exact(ExactVectorReason),
}

#[derive(Debug)]
pub struct NavigationVector {
    coordinates: BudgetedVec<f64>,
}

/// Nonnegative squared Euclidean distance, with no conversion to a score or probability carrier.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct SquaredNavigationDistance(f64);

impl SquaredNavigationDistance {
    pub fn get(self) -> f64 {
        self.0
    }
}

impl NavigationInput {
    pub fn from_raw(
        dimensions: u32,
        raw: &[f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let (canonical_norm, norm) = norms(dimensions, raw, control)?;
        if let Some(reason) = exact_reason(canonical_norm) {
            return Ok(Self::Exact(reason));
        }
        let mut coordinates = BudgetedVec::new(control.memory());
        coordinates.reserve(raw.len())?;
        for (offset, &value) in raw.iter().enumerate() {
            checkpoint(offset, control)?;
            coordinates.push(f64::from(value) / norm)?;
        }
        control.check()?;
        Ok(Self::Navigable(NavigationVector { coordinates }))
    }
}

pub(super) fn exact_reason(canonical_norm: f32) -> Option<ExactVectorReason> {
    if canonical_norm == 0.0 {
        Some(ExactVectorReason::ZeroNorm)
    } else if !canonical_norm.is_finite() {
        Some(ExactVectorReason::NonFiniteNorm)
    } else {
        None
    }
}

pub(super) fn norms(
    dimensions: u32,
    raw: &[f32],
    control: &StorageReadControl,
) -> StorageBackendResult<(f32, f64)> {
    validate_vector_values_controlled(dimensions, raw, Some(control))?;
    if dimensions == 0 {
        return Err(StorageBackendError::Other(
            "DiskANN dimensions must be positive".into(),
        ));
    }
    let mut canonical_squared_norm = 0.0_f32;
    let mut squared_norm = 0.0_f64;
    for (offset, &value) in raw.iter().enumerate() {
        checkpoint(offset, control)?;
        canonical_squared_norm += value * value;
        let value = f64::from(value);
        squared_norm += value * value;
    }
    Ok((canonical_squared_norm.sqrt(), squared_norm.sqrt()))
}

impl NavigationVector {
    pub fn coordinates(&self) -> &[f64] {
        &self.coordinates
    }

    pub fn squared_distance(
        &self,
        other: &Self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SquaredNavigationDistance> {
        control.check()?;
        if self.coordinates.len() != other.coordinates.len() {
            return Err(StorageBackendError::Other(
                "DiskANN navigation dimensions differ".into(),
            ));
        }
        Ok(SquaredNavigationDistance(squared_distance(
            &self.coordinates,
            &other.coordinates,
            control,
        )?))
    }
}

pub(super) fn squared_distance(
    left: &[f64],
    right: &[f64],
    control: &StorageReadControl,
) -> StorageBackendResult<f64> {
    debug_assert_eq!(left.len(), right.len());
    let mut distance = 0.0;
    for (offset, (left, right)) in left.iter().zip(right).enumerate() {
        checkpoint(offset, control)?;
        let difference = left - right;
        distance += difference * difference;
    }
    Ok(distance)
}

pub(super) fn checkpoint(offset: usize, control: &StorageReadControl) -> StorageBackendResult<()> {
    if offset.is_multiple_of(1024) {
        control.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
