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
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};
use uqa_sql::{plan::RetainedCtePlan, semantics::cte_strategy::ScheduledCte, SQLError};

type PostponedCtes = Produced<Vec<(RetainedCtePlan, bool)>>;

/// The commands of one statement and what they leave for its end, shared by every scope of the statement. `PostgreSQL` runs a statement under one command id and one AFTER trigger query level: the AFTER events of the statement's commands, its data-modifying WITH items and the referential actions those take among them, wait in one queue until the statement has finished (`AfterTriggerEndQuery`), and a row that one of its commands wrote is `TM_SelfModified` to the others.
#[derive(Default)]
pub struct StatementCommands {
    /// Whether the statement's WITH modifies data, which makes its commands leave their AFTER events for the statement's end.
    modifies_with: AtomicBool,
    /// The WITH items that run once the primary query has finished, in the order they run.
    postponed: parking_lot::Mutex<Option<PostponedCtes>>,
    /// The rows the statement's commands inserted, updated or deleted, which a statement whose WITH modifies data keeps.
    written: parking_lot::Mutex<BTreeSet<PhysicalDocumentIdentity>>,
    /// The rows that statements the statement started wrote: statements of its triggers and of the functions it calls, and their referential actions, which run under later command ids.
    triggered: parking_lot::Mutex<BTreeSet<PhysicalDocumentIdentity>>,
    after_triggers: AfterTriggerQueue,
    _memory: Option<MemoryReservation>,
}

impl StatementCommands {
    /// Admit the shared command owner before allocation. Its queues retain their own independently admitted payloads.
    pub(crate) fn with_control(
        control: &ProductionControl<'_>,
    ) -> Result<std::sync::Arc<Self>, SQLError> {
        let memory = control.reserve(size_of::<Self>())?;
        Ok(std::sync::Arc::new(Self {
            _memory: memory,
            ..Self::default()
        }))
    }

    /// Prepare the complete postponed schedule before changing this statement's command state. A failure leaves an existing schedule and its owners intact.
    pub(crate) fn begin_data_modifying_with<'a>(
        &self,
        postponed: impl IntoIterator<Item = ScheduledCte<'a>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut schedule = ProductionVec::new(*control);
        for entry in postponed {
            schedule.reserve(1)?;
            let plan = RetainedCtePlan::capture(entry.plan, control)?;
            schedule.push_produced(
                control.finish((plan, entry.deferred), control.empty_reservation())?,
            )?;
        }
        let schedule = schedule.finish()?;
        *self.postponed.lock() = Some(schedule);
        self.modifies_with.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn modifies_with(&self) -> bool {
        self.modifies_with.load(Ordering::Relaxed)
    }

    pub fn after_triggers(&self) -> &AfterTriggerQueue {
        &self.after_triggers
    }

    /// Move the once-only schedule together with its vector and immutable plan owners. Readers may share a selected plan without extending unrelated schedule storage.
    pub(crate) fn take_scheduled_postponed(&self) -> Option<PostponedCtes> {
        self.postponed.lock().take()
    }

    pub fn note_written(&self, rows: impl IntoIterator<Item = PhysicalDocumentIdentity>) {
        self.written.lock().extend(rows);
    }

    /// Whether one of the statement's commands wrote `row`, which the statement's later commands find in their snapshot as a row the statement already modified.
    pub fn wrote(&self, row: &PhysicalDocumentIdentity) -> bool {
        self.written.lock().contains(row)
    }

    pub fn note_triggered(&self, rows: impl IntoIterator<Item = PhysicalDocumentIdentity>) {
        self.triggered.lock().extend(rows);
    }

    /// Whether a statement that this statement started wrote `row`, which `PostgreSQL` finds modified by a later command than the statement's own (`TM_SelfModified` with another `cmax`).
    pub fn triggered_write(&self, row: &PhysicalDocumentIdentity) -> bool {
        self.triggered.lock().contains(row)
    }
}

#[cfg(test)]
mod tests;
