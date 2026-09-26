//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` numerical and physical primitives. Navigation values are not public retrieval scores.

pub mod build;
mod canonical;
pub mod catalog;
pub mod changes;
pub mod format;
pub mod maintenance;
mod memory;
mod metric;
mod options;
pub mod pages;
mod persistent;
mod pq;
mod query;
mod random;
mod scoring;
pub mod search;
mod vamana;

pub use canonical::{DiskANNCanonicalCorpusVisitor, DiskANNCanonicalRead, DiskANNQueryRead};
pub use memory::{DiskANNMemoryIndex, DiskANNMemoryOptions};
pub use metric::{ExactVectorReason, NavigationInput, NavigationVector, SquaredNavigationDistance};
pub use options::DiskANNIndexOptions;
pub use persistent::{DiskANNIndexBinding, DiskANNPersistentOwner, PersistentDiskANNIndex};
pub use pq::{
    PQCodebook, PQDistance, PQLookupTable, PQTrainer, PQTrainingOptions, PQTrainingSummary,
};
pub use query::{DiskANNQuery, DiskANNQueryResult, RetainedDiskANNIndex};
pub use scoring::{DiskANNCanonicalScorer, DiskANNDocumentScore};
pub use vamana::{VamanaGraph, VamanaPoint};

/// Borrow one canonical tensor ordinal on a fixed source. A failed visit invalidates the caller's partial result.
pub type DiskANNCanonicalVectorVisitor<'a> =
    dyn FnMut(u32, format::DiskANNVectorVersion, &[f32]) -> crate::StorageBackendResult<()> + 'a;
