//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind unique-index row validation to the active storage generation.
use crate::Engine;
use uqa_sql::{ast::CreateIndex, SQLError};
pub(super) fn validate_unique_index(
    engine: &Engine,
    statement: &CreateIndex,
    name: &str,
) -> Result<(), SQLError> {
    let runtime = engine.query_runtime_view();
    uqa_execution::schema::indexes::validate_unique_index(
        &uqa_execution::schema::indexes::IndexBuildContext {
            catalog: engine,
            reads: engine,
            expressions: engine.constraint_execution_context().index_expressions(),
            memory: runtime.settings,
        },
        statement,
        name,
    )
}
