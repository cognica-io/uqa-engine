//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph records use Storage's common spilling ordered map.

mod node;

pub(super) use crate::spill_map::{invalid, Iter, Map, Read, Record};
