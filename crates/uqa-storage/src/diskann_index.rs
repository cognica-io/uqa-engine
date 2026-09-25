//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` numerical primitives. Navigation values are not public retrieval scores.

mod metric;
mod pq;
mod random;

pub use metric::{ExactVectorReason, NavigationInput, NavigationVector, SquaredNavigationDistance};
pub use pq::{
    PQCodebook, PQDistance, PQLookupTable, PQTrainer, PQTrainingOptions, PQTrainingSummary,
};
