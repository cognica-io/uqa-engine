//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The AFTER events of an UPDATE, fired generation by generation with the referential actions it took.

use crate::mutation::events::MutationEventQueue;
use crate::mutation::triggers::context::TriggerContext;
use uqa_sql::SQLError;

/// Fire the AFTER row events an UPDATE of `table` queued and, when the statement's own query survived its rules, the AFTER STATEMENT triggers of `table` for `assigned_columns`.
pub fn fire_update_after_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    update_original_query: bool,
    assigned_columns: &[String],
    events: &MutationEventQueue,
) -> Result<(), SQLError> {
    let transition_tables = if update_original_query {
        crate::mutation::triggers::build_transition_tables(
            context,
            table,
            uqa_sql::ast::TriggerEvent::Update,
            assigned_columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(context)?;
    let mut transition_refs = transition_tables.iter().collect::<Vec<_>>();
    transition_refs.extend(referential_transition.iter());
    let root_events = update_original_query
        .then_some(uqa_sql::ast::TriggerEvent::Update)
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
        if update_original_query {
            crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                context,
                table,
                uqa_sql::ast::TriggerEvent::Update,
                assigned_columns,
                &transition_tables,
                generation,
            )?;
        }
    }
    Ok(())
}
