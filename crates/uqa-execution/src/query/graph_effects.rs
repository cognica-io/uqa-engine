//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Classify an embedded graph-language query using its owning parser.

use uqa_sql::SQLError;

pub fn query_is_mutating(query: &str) -> Result<bool, SQLError> {
    uqa_graph::cypher::parse_cypher(query)
        .map(|query| query.mutates_graph())
        .map_err(|error| SQLError::Unsupported(format!("cypher: {error}")))
}
