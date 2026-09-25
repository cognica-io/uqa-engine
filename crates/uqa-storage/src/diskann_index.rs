//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` numerical and physical primitives. Navigation values are not public retrieval scores.

pub mod build;
mod canonical;
pub mod format;
mod metric;
pub mod pages;
mod pq;
mod random;
pub mod search;
mod vamana;

pub use canonical::{DiskANNCanonicalCorpusVisitor, DiskANNCanonicalRead};
pub use metric::{ExactVectorReason, NavigationInput, NavigationVector, SquaredNavigationDistance};
pub use pq::{
    PQCodebook, PQDistance, PQLookupTable, PQTrainer, PQTrainingOptions, PQTrainingSummary,
};
pub use vamana::{VamanaGraph, VamanaPoint};

/// Borrow one canonical tensor ordinal on a fixed source. A failed visit invalidates the caller's partial result.
pub type DiskANNCanonicalVectorVisitor<'a> =
    dyn FnMut(u32, format::DiskANNVectorVersion, &[f32]) -> crate::StorageBackendResult<()> + 'a;
