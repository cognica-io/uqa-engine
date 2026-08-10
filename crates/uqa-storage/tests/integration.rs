//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated storage integration tests.

#[path = "btree_index.rs"]
mod btree_index;
#[path = "catalog.rs"]
mod catalog;
#[path = "inverted_index_analyzer.rs"]
mod inverted_index_analyzer;
#[path = "skip_blockmax_coverage.rs"]
mod skip_blockmax_coverage;
#[path = "sqlite_document_store.rs"]
mod sqlite_document_store;
