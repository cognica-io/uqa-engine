//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply immutable catalog inputs to SQL-owned stored-statement binding.

use crate::Engine;
pub(crate) use uqa_sql::binding::stored_routines::{
    collect_expression_routine_references, mark_catalog_statement_relations_bound,
    BoundStatementRoutines,
};
use uqa_sql::{plan::UnifiedPlan, SQLError};

pub(crate) fn bind_catalog_statement_routines(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<BoundStatementRoutines, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::binding::stored_routines::bind_catalog_statement_routines(
        &uqa_sql::binding::stored_routines::CatalogRoutineContext {
            routines: engine,
            binding: &binding,
        },
        plan,
    )
}
