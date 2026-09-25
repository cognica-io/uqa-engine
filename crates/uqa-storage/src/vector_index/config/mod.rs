//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical vector-index configuration shared by engines and backends.

mod diskann;
mod hnsw;
mod ivf;
mod parsing;
mod types;

pub use diskann::{DiskANNAlpha, DiskANNIndexParams};
pub use hnsw::HNSWIndexParams;
pub use ivf::IVFIndexParams;
pub use types::{VectorIndexOpenMode, VectorIndexSpec};

#[cfg(test)]
mod tests;
