//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply SQL-owned read-only classification to the current transaction.

use crate::Engine;
pub(super) use uqa_sql::semantics::effects::read_only::plan_sets_transaction_snapshot;
use uqa_sql::semantics::effects::read_only::read_only_error;
use uqa_sql::{plan::UnifiedPlan, SQLError};

pub(super) fn validate_transaction_plan(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    if engine.current_transaction_is_read_only() {
        if let Some(command) = forbidden_command(engine, plan)? {
            return Err(read_only_error(command));
        }
    }
    if plan_sets_transaction_snapshot(plan) {
        engine.mark_transaction_snapshot_set();
    }
    Ok(())
}

pub(super) fn forbidden_command(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<Option<&'static str>, SQLError> {
    uqa_sql::semantics::effects::read_only::forbidden_command(&engine.query_effect_context(), plan)
}
