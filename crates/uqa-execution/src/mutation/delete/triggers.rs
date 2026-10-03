//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The AFTER events of a DELETE, fired generation by generation with the referential actions it took.

use crate::mutation::events::MutationEventQueue;
use crate::mutation::triggers::context::TriggerContext;
use uqa_sql::SQLError;

/// Fire the AFTER row events a DELETE from `table` queued and, when the statement's own query survived its rules, the AFTER STATEMENT triggers of `table`.
pub(super) fn fire_delete_after_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    delete_original_query: bool,
    events: &MutationEventQueue,
) -> Result<(), SQLError> {
    let transition_tables = if delete_original_query {
        crate::mutation::triggers::build_transition_tables(
            context,
            table,
            uqa_sql::ast::TriggerEvent::Delete,
            &[],
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(context)?;
    let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
    transition_refs.extend(referential_transition.iter());
    let root_events = delete_original_query
        .then_some(uqa_sql::ast::TriggerEvent::Delete)
        .into_iter()
        .collect::<Vec<_>>();
    for generation in crate::mutation::triggers::after_trigger_generations(&transition_refs) {
        crate::mutation::triggers::fire_after_row_trigger_events_for_generation(
            context,
            events.after_rows(),
            &transition_refs,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            context,
            &referential_transition,
            table,
            &root_events,
            generation,
        )?;
        if delete_original_query {
            crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                context,
                table,
                uqa_sql::ast::TriggerEvent::Delete,
                &[],
                &transition_tables,
                generation,
            )?;
        }
    }
    Ok(())
}
