//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` IVF index split into lifecycle, training, mutation, persistence,
//! and search responsibilities.

use super::SQLiteVectorIndex;
use crate::vector_index::IVFIndexParams;

mod lifecycle;
mod loading;
mod math;
mod metadata;
mod mutation;
mod search;
mod training;
mod writing;

#[derive(Clone)]
pub struct SQLiteIVFIndex {
    pub(super) persistent: SQLiteVectorIndex,
    pub(super) params: IVFIndexParams,
}
