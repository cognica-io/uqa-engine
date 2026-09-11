//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind engine state to execution-owned trigger dispatch.

use crate::Engine;
pub(crate) use uqa_execution::mutation::triggers::{
    current_transition_relation_names, current_transition_relations, DeferredConstraintTriggerEvent,
};
use uqa_sql::{
    ast::{TriggerEvent, TriggerTiming},
    error::Result,
};

pub(crate) fn fire_statement_triggers(
    engine: &Engine,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<()> {
    uqa_execution::mutation::triggers::fire_statement_triggers(
        &engine.trigger_execution_context(),
        table,
        timing,
        event,
        updated_columns,
    )
}

pub(crate) fn fire_deferred_constraint_trigger_event(
    engine: &Engine,
    event: &DeferredConstraintTriggerEvent,
) -> Result<()> {
    uqa_execution::mutation::triggers::fire_deferred_constraint_trigger_event(
        &engine.trigger_execution_context(),
        event,
    )
}
