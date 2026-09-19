//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory inverted-file vector index.
//!
//! The implementation is separated by responsibility: graph-independent
//! state, vector math, training, mutation, querying, and the public
//! [`crate::VectorIndex`] adapter. Below the training threshold queries scan
//! all vectors; trained indexes probe only the nearest centroid lists.

#![allow(clippy::cast_lossless, clippy::similar_names)]

mod index;
mod math;
mod mutation;
mod prepare;
mod restore;
mod search;
mod state;
mod training;

pub use prepare::IVFMutation;
pub use state::IVFMetadataSnapshot;
pub use state::{IVFIndex, IVFState};

#[cfg(test)]
#[path = "tests/preparation.rs"]
mod preparation_tests;
#[cfg(test)]
mod tests;
