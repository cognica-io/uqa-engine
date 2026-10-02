//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The row triggers a statement fires, resolved once for each table and event it writes.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use uqa_sql::{
    ast::{TriggerEvent, TriggerTiming},
    catalog::events::StoredTrigger,
    error::Result,
};

use super::context::TriggerContext;

/// The triggers of one table for a timing, an event and the columns an update names.
struct Resolved {
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: Vec<String>,
    triggers: Rc<[StoredTrigger]>,
}

thread_local! {
    /// What each statement in progress on this thread has resolved for the tables it writes, innermost statement last.
    static STATEMENTS: RefCell<Vec<HashMap<String, Vec<Resolved>>>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn enter() {
    STATEMENTS.with(|statements| statements.borrow_mut().push(HashMap::new()));
}

pub(super) fn leave() {
    STATEMENTS.with(|statements| {
        let removed = statements.borrow_mut().pop();
        debug_assert!(removed.is_some(), "row trigger scope stack underflow");
    });
}

/// The row triggers of `table` for `timing` and `event`, whatever replication role fires them.
///
/// Resolving them names the table, walks its partition ancestors and copies the definitions, and a statement asks before and after every row it writes. `PostgreSQL` copies a relation's trigger descriptor when a statement begins to write the relation (`InitResultRelInfo`), so a statement fires the triggers that existed then: one created while it runs waits for the next statement, and one dropped while it runs still fires for the rest of it. A statement therefore resolves once for each table, timing, event and set of updated columns. Outside a statement every call resolves.
pub(super) fn resolve(
    context: &TriggerContext<'_>,
    table: &str,
    timing: TriggerTiming,
    event: TriggerEvent,
    updated_columns: &[String],
) -> Result<Rc<[StoredTrigger]>> {
    let known = STATEMENTS.with(|statements| {
        let statements = statements.borrow();
        statements
            .last()?
            .get(table)?
            .iter()
            .find(|resolved| {
                resolved.timing == timing
                    && resolved.event == event
                    && resolved.updated_columns == updated_columns
            })
            .map(|resolved| Rc::clone(&resolved.triggers))
    });
    if let Some(triggers) = known {
        return Ok(triggers);
    }
    let triggers: Rc<[StoredTrigger]> = context
        .catalog
        .row_trigger_definitions(table, timing, event, updated_columns)?
        .into();
    STATEMENTS.with(|statements| {
        if let Some(statement) = statements.borrow_mut().last_mut() {
            statement
                .entry(table.to_owned())
                .or_default()
                .push(Resolved {
                    timing,
                    event,
                    updated_columns: updated_columns.to_vec(),
                    triggers: Rc::clone(&triggers),
                });
        }
    });
    Ok(triggers)
}

/// Whether `trigger` fires in the session's replication role as it stands now. `PostgreSQL` reads the role at each firing (`TriggerEnabled`), so a trigger that changes it decides for every trigger after it, of its own row as well.
pub(super) fn fires(context: &TriggerContext<'_>, trigger: &StoredTrigger) -> bool {
    if context.referrers.session_replication_role_is_replica() {
        trigger.enabled.fires_in_replica()
    } else {
        trigger.enabled.fires_in_origin()
    }
}
