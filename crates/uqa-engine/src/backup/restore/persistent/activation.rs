//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Open the durably installed database in bounded catalog steps.

use super::{catalog_limits, AdmittedRestore, RestoredEngine};
use crate::{backup::RestoreBindings, open::PersistentEngineInput};
use std::path::Path;
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedValue};
use uqa_storage::{
    backup::{BackupResult, PersistentBackupTarget},
    mvcc::DatabaseRestore,
    read_control::StorageReadControl,
};

pub(in crate::backup::restore) struct PublishedInput {
    input: Budgeted<Box<PersistentEngineInput>>,
    // Keep the validated registrations stable until the writable session is returned.
    _bindings: RestoreBindings,
}

impl PublishedInput {
    pub(super) fn open(
        target: &dyn PersistentBackupTarget,
        directory: &Path,
        request: DatabaseRestore,
        options: &AdmittedRestore,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let completion = control.for_durable_completion();
        let memory = completion
            .memory()
            .reserve(size_of::<PersistentEngineInput>())?;
        let provider = target.open(directory, request, &completion)?;
        let input = PersistentEngineInput::new(
            provider,
            retained,
            catalog_limits(options.limits),
            &options.bindings,
            &completion,
        )?;
        Ok(Self {
            input: Budgeted::new(Box::new(input), memory),
            _bindings: options.bindings.clone(),
        })
    }

    pub(in crate::backup::restore) fn advance(
        &mut self,
        maximum_steps: usize,
    ) -> BackupResult<Option<RestoredEngine>> {
        let Some(session) = self.input.as_mut().advance(maximum_steps)? else {
            return Ok(None);
        };
        let engine = session.activate()?;
        Ok(Some(RestoredEngine(RetainedValue::with_budget(
            engine,
            self.input.budget(),
        )?)))
    }

    pub(super) fn finish(mut self) -> BackupResult<RestoredEngine> {
        loop {
            if let Some(engine) = self.advance(64)? {
                return Ok(engine);
            }
        }
    }
}
