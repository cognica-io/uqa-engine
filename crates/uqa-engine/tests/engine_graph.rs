//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated graph, Cypher, AGE, RPQ, and tensor integration tests.

#[path = "age_agtype_compat.rs"]
mod age_agtype_compat;
#[path = "cypher_engine.rs"]
mod cypher_engine;
#[path = "graph_delta_and_path_index.rs"]
mod graph_delta_and_path_index;
#[path = "graph_path_index_persistence.rs"]
mod graph_path_index_persistence;
#[path = "postgres_age_compat.rs"]
mod postgres_age_compat;
#[path = "sql_graph_functions.rs"]
mod sql_graph_functions;
#[path = "sql_graph_lifecycle.rs"]
mod sql_graph_lifecycle;
#[path = "sql_rpq.rs"]
mod sql_rpq;
#[path = "sql_tensor.rs"]
mod sql_tensor;
