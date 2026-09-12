//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated storage integration tests.

#[path = "cases/analyzer_bindings.rs"]
mod analyzer_bindings;
#[path = "btree_index.rs"]
mod btree_index;

#[path = "cases/occurrences.rs"]
mod occurrences;

#[path = "cases/memory_occurrences.rs"]
mod memory_occurrences;
