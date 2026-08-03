//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common contract every index implementation honours.
//!
//! An [`Index`] supports
//! predicate-driven scans, scan cost estimation, build, and drop.
//! The storage-layer concrete types ([`crate::btree_index::BTreeIndex`],
//! the SQLite-backed inverted index, the IVF index, ...) all
//! implement this trait so the planner / [`crate::IndexManager`] can
//! pick one without monomorphising.

use uqa_core::{PostingList, Predicate};

use crate::index_types::IndexDef;

/// Predicate-aware index. Concrete implementations live in their own
/// modules; the planner reaches them through trait objects so it can
/// choose between candidate indexes by cost without compile-time
/// monomorphisation.
pub trait Index: Send + Sync {
    fn index_def(&self) -> &IndexDef;
    fn scan(&self, predicate: &Predicate) -> PostingList;
    fn estimate_cardinality(&self, predicate: &Predicate) -> usize;
    fn scan_cost(&self, predicate: &Predicate) -> f64;
    fn build(&mut self) -> Result<(), crate::SQLiteError>;
    /// Tear down the physical index. Avoids the name `drop` so it does
    /// not collide with [`Drop::drop`] when the trait is invoked
    /// through a trait object.
    fn drop_index(&mut self) -> Result<(), crate::SQLiteError>;
}
