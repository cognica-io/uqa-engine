//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The commands of one statement and what they leave for the statement's end.

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::mutation::triggers::queue::AfterTriggerQueue;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use uqa_sql::plan::CtePlan;

/// The commands of one statement and what they leave for its end, shared by every scope of the statement. `PostgreSQL` runs a statement under one command id and one AFTER trigger query level: the AFTER events of the statement's commands, its data-modifying WITH items and the referential actions those take among them, wait in one queue until the statement has finished (`AfterTriggerEndQuery`), and a row that one of its commands wrote is `TM_SelfModified` to the others.
#[derive(Default)]
pub struct StatementCommands {
    /// Whether the statement's WITH modifies data, which makes its commands leave their AFTER events for the statement's end.
    modifies_with: AtomicBool,
    /// The WITH items that run once the primary query has finished, in the order they run.
    postponed: parking_lot::Mutex<Vec<CtePlan>>,
    /// The rows the statement's commands inserted, updated or deleted, which a statement whose WITH modifies data keeps.
    written: parking_lot::Mutex<BTreeSet<PhysicalDocumentIdentity>>,
    after_triggers: AfterTriggerQueue,
}

impl StatementCommands {
    /// Note that the statement's WITH modifies data, and keep `postponed`, the items that run once the primary query has finished.
    pub(crate) fn begin_data_modifying_with(&self, postponed: Vec<CtePlan>) {
        self.modifies_with.store(true, Ordering::Relaxed);
        *self.postponed.lock() = postponed;
    }

    pub fn modifies_with(&self) -> bool {
        self.modifies_with.load(Ordering::Relaxed)
    }

    pub fn after_triggers(&self) -> &AfterTriggerQueue {
        &self.after_triggers
    }

    /// The items that run once the primary query has finished, which the statement runs once.
    pub fn take_postponed(&self) -> Vec<CtePlan> {
        std::mem::take(&mut *self.postponed.lock())
    }

    pub fn note_written(&self, rows: impl IntoIterator<Item = PhysicalDocumentIdentity>) {
        self.written.lock().extend(rows);
    }

    /// Whether one of the statement's commands wrote `row`, which the statement's later commands find in their snapshot as a row the statement already modified.
    pub fn wrote(&self, row: &PhysicalDocumentIdentity) -> bool {
        self.written.lock().contains(row)
    }
}
