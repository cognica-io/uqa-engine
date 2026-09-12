//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` provider integration tests.

#[path = "cases/analyzer_catalog.rs"]
mod analyzer_catalog;
#[path = "cases/analyzer_revisions.rs"]
mod analyzer_revisions;
#[path = "cases/catalog.rs"]
mod catalog;
#[path = "cases/inverted_index_analyzer.rs"]
mod inverted_index_analyzer;
#[path = "cases/persistent_graph.rs"]
mod persistent_graph;
#[path = "cases/skip_blockmax_coverage.rs"]
mod skip_blockmax_coverage;
#[path = "cases/sqlite_document_store.rs"]
mod sqlite_document_store;
