//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply rule definitions and session namespace inputs for SQL CTE validation.

use crate::Engine;
use uqa_sql::{plan::UnifiedPlan, SQLError};

pub(super) fn validate_plan(engine: &Engine, plan: &UnifiedPlan) -> Result<(), SQLError> {
    let resolution = engine.session_execution_view().relation_name_resolution();
    uqa_sql::semantics::cte_validation::validate_plan(
        &uqa_sql::semantics::cte_validation::CteValidationContext {
            catalog: engine,
            resolution: &resolution,
        },
        plan,
    )
}
