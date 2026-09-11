//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL Cypher execution to live graph state and the public graph transaction boundary.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_execution::query::cypher::CypherTableRuntime;
use uqa_graph::cypher::{CypherError, ResultRow};
use uqa_storage::StorageBackendResult;

use crate::Engine;

impl CypherTableRuntime for Engine {
    fn current_transaction_is_read_only(&self) -> bool {
        self.current_transaction_is_read_only()
    }

    fn has_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.has_graph(name)
    }

    fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        self.run_cypher(graph, query, params)
    }
}
