//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The data-modifying WITH items of one statement and what their commands leave for the statement's end.

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::mutation::triggers::context::TriggerContext;
use std::collections::BTreeSet;
use uqa_sql::plan::CtePlan;
use uqa_sql::SQLError;

/// The AFTER events one command queued, fired when the statement that contains the command ends.
pub type AfterEventFiring = Box<dyn FnOnce(&TriggerContext<'_>) -> Result<(), SQLError> + Send>;

/// The data-modifying WITH items of one statement and what their commands leave for the statement's end, shared by every scope of the statement. `PostgreSQL` runs the whole statement under one command id: its AFTER events wait in one queue until the primary query and every item have finished (`AfterTriggerEndQuery`), and a row that one of its commands wrote is `TM_SelfModified` to the others.
#[derive(Default)]
pub struct StatementCommands {
    /// The items that run once the primary query has finished, in the order they run.
    postponed: parking_lot::Mutex<Vec<CtePlan>>,
    /// The AFTER events the statement's commands queued, in queue order.
    after_events: parking_lot::Mutex<Vec<AfterEventFiring>>,
    /// The rows the statement's commands inserted, updated or deleted.
    written: parking_lot::Mutex<BTreeSet<PhysicalDocumentIdentity>>,
}

impl StatementCommands {
    pub(crate) fn new(postponed: Vec<CtePlan>) -> Self {
        Self {
            postponed: parking_lot::Mutex::new(postponed),
            ..Self::default()
        }
    }

    /// The items that run once the primary query has finished, which the statement runs once.
    pub fn take_postponed(&self) -> Vec<CtePlan> {
        std::mem::take(&mut *self.postponed.lock())
    }

    pub fn queue_after_events(&self, fire: AfterEventFiring) {
        self.after_events.lock().push(fire);
    }

    /// The queued AFTER events, in the order they fire.
    pub fn take_after_events(&self) -> Vec<AfterEventFiring> {
        std::mem::take(&mut *self.after_events.lock())
    }

    pub fn note_written(&self, rows: impl IntoIterator<Item = PhysicalDocumentIdentity>) {
        self.written.lock().extend(rows);
    }

    /// Whether one of the statement's commands wrote `row`, which the statement's later commands find in their snapshot as a row the statement already modified.
    pub fn wrote(&self, row: &PhysicalDocumentIdentity) -> bool {
        self.written.lock().contains(row)
    }
}
