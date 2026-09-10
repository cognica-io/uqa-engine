//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mutation::triggers::context::TriggerContext;
use uqa_sql::SQLError;
pub fn fire_insert_after_triggers(
    context: &TriggerContext<'_>,
    table: &str,
    insert_original_query: bool,
    conflict_update_columns: Option<&[String]>,
    events: &crate::mutation::events::MutationEventQueue,
) -> Result<(), SQLError> {
    let insert_transition = if insert_original_query {
        crate::mutation::triggers::build_transition_tables(
            context,
            table,
            uqa_sql::ast::TriggerEvent::Insert,
            &[],
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let update_transition = if let Some(columns) = conflict_update_columns {
        crate::mutation::triggers::build_transition_tables(
            context,
            table,
            uqa_sql::ast::TriggerEvent::Update,
            columns,
            events.after_rows(),
        )?
    } else {
        Vec::new()
    };
    let referential_transition = events.referential_transition_tables(context)?;
    let mut transition_tables = insert_transition
        .iter()
        .chain(update_transition.iter())
        .collect::<Vec<_>>();
    transition_tables.extend(referential_transition.iter());
    let mut root_events = Vec::new();
    if conflict_update_columns.is_some() {
        root_events.push(uqa_sql::ast::TriggerEvent::Update);
    }
    if insert_original_query {
        root_events.push(uqa_sql::ast::TriggerEvent::Insert);
    }
    for generation in crate::mutation::triggers::after_trigger_generations(&transition_tables) {
        crate::mutation::triggers::fire_after_row_trigger_events_for_generation(
            context,
            events.after_rows(),
            &transition_tables,
            generation,
        )?;
        events.fire_referential_after_statement_triggers(
            context,
            &referential_transition,
            table,
            &root_events,
            generation,
        )?;
        if let Some(columns) = conflict_update_columns {
            crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                context,
                table,
                uqa_sql::ast::TriggerEvent::Update,
                columns,
                &update_transition,
                generation,
            )?;
        }
        if insert_original_query {
            crate::mutation::triggers::fire_after_statement_trigger_generation_for_root(
                context,
                table,
                uqa_sql::ast::TriggerEvent::Insert,
                &[],
                &insert_transition,
                generation,
            )?;
        }
    }
    Ok(())
}
