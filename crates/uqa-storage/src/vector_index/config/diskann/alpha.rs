//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::StorageBackendResult;

/// Euclidean pruning factor with validated, equality-stable IEEE-754 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNAlpha(u64);

impl DiskANNAlpha {
    pub fn new(value: f64) -> StorageBackendResult<Self> {
        if !value.is_finite() || value < 1.0 || !(value * value).is_finite() {
            return Err(super::invalid(
                "alpha",
                "must be finite, at least 1, and have a finite square",
            ));
        }
        Ok(Self(value.to_bits()))
    }

    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }

    pub fn squared(self) -> f64 {
        self.get() * self.get()
    }
}

impl Default for DiskANNAlpha {
    fn default() -> Self {
        Self(1.2_f64.to_bits())
    }
}
