//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` numerical primitives. Navigation values are not public retrieval scores.

mod metric;

pub use metric::{ExactVectorReason, NavigationInput, NavigationVector, SquaredNavigationDistance};
