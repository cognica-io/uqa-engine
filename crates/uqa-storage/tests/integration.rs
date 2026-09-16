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

#[path = "cases/key_value_occurrences.rs"]
mod key_value_occurrences;

#[path = "cases/field_metadata.rs"]
mod field_metadata;

#[path = "cases/japanese.rs"]
mod japanese;

#[path = "cases/mvcc.rs"]
mod mvcc;

#[path = "cases/mvcc_private.rs"]
mod mvcc_private;

#[path = "cases/mvcc_reads.rs"]
mod mvcc_reads;

#[path = "cases/mvcc_persistence.rs"]
mod mvcc_persistence;

#[path = "cases/mvcc_sessions.rs"]
mod mvcc_sessions;
