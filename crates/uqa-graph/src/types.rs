//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-crate-local types. Vertex and Edge themselves live in
//! [`uqa_core::types`] so any crate can name them without depending on
//! `uqa-graph`.

/// Edge traversal direction relative to a vertex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Outgoing edges (`edge.source_id == vertex`).
    Out,
    /// Incoming edges (`edge.target_id == vertex`).
    In,
    /// Both directions, deduplicated.
    Both,
}
