//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph records use Storage's common spilling ordered map.

mod node;
mod vector;

pub(super) use crate::spill_map::{invalid, Iter, Map, Read, Record};

#[cfg(test)]
thread_local! {
    pub(super) static DECODED_VECTOR_FLOATS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ENCODED_NODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ENCODED_VECTORS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
