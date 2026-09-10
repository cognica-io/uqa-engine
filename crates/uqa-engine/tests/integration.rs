//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// The required single libtest harness has more than 2,048 generated test descriptors.
#![allow(clippy::large_stack_arrays)]

//! Single integration-test executable for every engine test domain.

#[path = "catalog.rs"]
mod catalog;
#[path = "functions.rs"]
mod functions;
#[path = "graph.rs"]
mod graph;
#[path = "queries.rs"]
mod queries;
#[path = "search.rs"]
mod search;
#[path = "sql_tpch.rs"]
mod sql_tpch;
#[path = "storage.rs"]
mod storage;
