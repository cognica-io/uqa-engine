//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind engine state to execution-owned trigger dispatch.

use crate::Engine;
use uqa_core::{DocId, Value};
pub(crate) use uqa_execution::mutation::triggers::{
    after_trigger_generations, current_transition_relation_names, current_transition_relations,
    enter_empty_transition_relation_scope, AfterRowTriggerEvent, AfterRowTriggerInput,
    DeferredConstraintTriggerEvent, TransitionCaptureScope, TransitionTables,
};
use uqa_sql::{
    ast::{TriggerEvent, TriggerTiming},
    error::Result,
};
use uqa_storage::document_store::Document;

pub(super) fn fire_instead_of_row_triggers(
    engine: &Engine,
    view: &str,
    event: TriggerEvent,
    old_values: Option<&[Value]>,
    new_values: Option<&[Value]>,
    updated_columns: &[String],
) -> Result<Option<Vec<Value>>> {
    uqa_execution::mutation::triggers::fire_instead_of_row_triggers(
        &engine.trigger_execution_context(),
        view,
        event,
        old_values,
        new_values,
        updated_columns,
    )
}

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

pub(super) fn fire_before_row_triggers(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    doc_id: DocId,
    old_document: Option<&Document>,
    new_document: Option<&Document>,
    updated_columns: &[String],
) -> Result<Option<Document>> {
    uqa_execution::mutation::triggers::fire_before_row_triggers(
        &engine.trigger_execution_context(),
        table,
        event,
        doc_id,
        old_document,
        new_document,
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

pub(super) fn fire_after_row_trigger_events_for_generation(
    engine: &Engine,
    events: &[AfterRowTriggerEvent],
    transition_tables: &[&TransitionTables],
    generation: usize,
) -> Result<()> {
    uqa_execution::mutation::triggers::fire_after_row_trigger_events_for_generation(
        &engine.trigger_execution_context(),
        events,
        transition_tables,
        generation,
    )
}

pub(super) fn fire_after_statement_trigger_generation_for_root(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    transition_tables: &[TransitionTables],
    generation: usize,
) -> Result<()> {
    uqa_execution::mutation::triggers::fire_after_statement_trigger_generation_for_root(
        &engine.trigger_execution_context(),
        table,
        event,
        updated_columns,
        transition_tables,
        generation,
    )
}

pub(in crate::sql) fn build_transition_tables(
    engine: &Engine,
    table: &str,
    event: TriggerEvent,
    updated_columns: &[String],
    events: &[AfterRowTriggerEvent],
) -> Result<Vec<TransitionTables>> {
    uqa_execution::mutation::triggers::build_transition_tables(
        &engine.trigger_execution_context(),
        table,
        event,
        updated_columns,
        events,
    )
}
