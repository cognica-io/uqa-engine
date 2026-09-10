//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize a mutation's repeatable source in the execution layer.

use super::{CteScope, Engine, SQLError, SQLParam, SourcePlan};

pub(in crate::sql) fn build_join_spill_with_ctes(
    engine: &Engine,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    uqa_execution::query::sources::build_join_spill_with_ctes(
        &engine.source_execution_context(),
        source,
        params,
        ctes,
    )
}
