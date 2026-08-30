//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// The required single libtest harness has more than 2,048 generated test descriptors.
#![allow(clippy::large_stack_arrays)]

//! Single integration-test executable for every engine test domain.

#[path = "engine_catalog.rs"]
mod engine_catalog;
#[path = "engine_functions.rs"]
mod engine_functions;
#[path = "engine_graph.rs"]
mod engine_graph;
#[path = "engine_queries.rs"]
mod engine_queries;
#[path = "engine_search.rs"]
mod engine_search;
#[path = "engine_storage.rs"]
mod engine_storage;
#[path = "sql_tpch.rs"]
mod sql_tpch;
