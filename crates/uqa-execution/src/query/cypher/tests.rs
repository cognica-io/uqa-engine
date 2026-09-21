//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL Cypher reports transaction/resource failures from both catalog and graph execution.

use super::*;
use uqa_storage::{mvcc::VersionError, StorageBackendError};

struct FailingGraph {
    catalog: bool,
    state: &'static str,
}

impl FailingGraph {
    fn error(&self) -> StorageBackendError {
        match self.state {
            "57014" => uqa_core::QueryCancelled.into(),
            "53200" => uqa_core::memory::MemoryError::SizeOverflow.into(),
            "40001" => VersionError::WriteConflict {
                mutation: 0,
                expected: None,
                actual: None,
            }
            .into_storage_error(),
            _ => unreachable!(),
        }
    }
}

impl CypherTableRuntime for FailingGraph {
    fn current_transaction_is_read_only(&self) -> bool {
        false
    }

    fn has_graph(&self, _: &str) -> StorageBackendResult<bool> {
        if self.catalog {
            Err(self.error())
        } else {
            Ok(true)
        }
    }

    fn run_cypher(
        &self,
        _: &str,
        _: &str,
        _: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        Err(uqa_graph::GraphStoreError::from(self.error()).into())
    }
}

#[test]
fn sql_cypher_preserves_failures_from_catalog_and_graph_execution() {
    for catalog in [false, true] {
        for state in ["57014", "53200", "40001"] {
            let error = build_rows(
                &FailingGraph { catalog, state },
                &[],
                &[
                    Value::Str("g".into()),
                    Value::Str("MATCH (n) RETURN n".into()),
                ],
                &["n".into()],
                &["agtype".into()],
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{error}");
        }
    }
}
