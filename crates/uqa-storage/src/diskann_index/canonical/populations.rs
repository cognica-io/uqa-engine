//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact canonical populations maintained with their selected immutable roots.

use crate::{mvcc::VersionError, StorageBackendResult};

mod persistent;
pub use persistent::{DiskANNPopulationState, DiskANNPopulationWitness};

/// Complete current ordinals and the subset whose origins are not covered by the selected physical generation. Empty tensors contribute zero to both populations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNCanonicalCounts {
    current_vectors: u64,
    changed_vectors: u64,
}

impl DiskANNCanonicalCounts {
    pub fn new(current_vectors: u64, changed_vectors: u64) -> StorageBackendResult<Self> {
        if changed_vectors > current_vectors {
            return Err(invalid());
        }
        Ok(Self {
            current_vectors,
            changed_vectors,
        })
    }

    pub fn current_vectors(self) -> u64 {
        self.current_vectors
    }

    pub fn changed_vectors(self) -> u64 {
        self.changed_vectors
    }

    /// Replace one complete tensor with a fresh, uncovered origin. The owner supplies its exact preceding cardinality and coverage classification on this same generation.
    pub(crate) fn replaced(
        self,
        previous_vectors: u64,
        previous_changed: bool,
        replacement_vectors: u64,
    ) -> StorageBackendResult<Self> {
        let current_vectors = self
            .current_vectors
            .checked_sub(previous_vectors)
            .and_then(|count| count.checked_add(replacement_vectors))
            .ok_or_else(invalid)?;
        let changed_vectors = self
            .changed_vectors
            .checked_sub(if previous_changed {
                previous_vectors
            } else {
                0
            })
            .and_then(|count| count.checked_add(replacement_vectors))
            .ok_or_else(invalid)?;
        Self::new(current_vectors, changed_vectors)
    }

    pub(crate) fn covered(self) -> Self {
        Self {
            changed_vectors: 0,
            ..self
        }
    }
}

fn invalid() -> crate::StorageBackendError {
    VersionError::InvalidEncoding("invalid DiskANN canonical population arithmetic")
        .into_storage_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diskann_population_arithmetic_rejects_invalid_subsets_and_overflow() {
        assert!(DiskANNCanonicalCounts::new(2, 3).is_err());
        assert!(DiskANNCanonicalCounts::new(u64::MAX, 0)
            .unwrap()
            .replaced(0, false, 1)
            .is_err());
        let counts = DiskANNCanonicalCounts::new(3, 1).unwrap();
        assert!(counts.replaced(4, false, 0).is_err());
        assert!(counts.replaced(2, true, 0).is_err());
        assert!(counts.replaced(3, false, 0).is_err());
        assert_eq!(counts.covered(), DiskANNCanonicalCounts::new(3, 0).unwrap());
    }
}
