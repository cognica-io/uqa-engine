//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial participant admission and safe read-only snapshot selection, before application execution.

use std::{sync::Arc, time::Duration};

use super::Transaction;
use crate::mvcc::{
    SafeSnapshot, SerializableReadContext, SerializableSnapshotCapture,
    SerializableSnapshotOptions, VersionError, VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;

impl Transaction {
    pub(in crate::mvcc::session) fn establish_serializable(
        &mut self,
        persistence: &Arc<dyn VersionedPersistence>,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableReadContext> {
        self.establish_serializable_with(
            persistence,
            SerializableSnapshotOptions {
                read_only: self.read_only,
                deferrable: false,
            },
            &mut |capture| capture(),
            control,
        )
    }

    pub(in crate::mvcc::session) fn establish_serializable_with(
        &mut self,
        persistence: &Arc<dyn VersionedPersistence>,
        options: SerializableSnapshotOptions,
        capture: &mut SerializableSnapshotCapture<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableReadContext> {
        self.unsealed()?;
        if let Some(context) = &self.serializable {
            context.with_graph(control, |graph| graph.check_active(context.id()))?;
            return Ok(context.clone());
        }
        if self.changes.has_written() || self.has_derived_changes() {
            return Err(VersionError::InvalidEncoding(
                "serializable snapshot must precede private writes",
            ));
        }
        let coordinator =
            persistence
                .serializable_coordinator()
                .ok_or(VersionError::InvalidEncoding(
                    "persistence has no serializable coordinator",
                ))?;
        loop {
            control.check()?;
            let mut candidate = None;
            let mut invoked = false;
            capture(&mut || {
                if std::mem::replace(&mut invoked, true) {
                    return Err(VersionError::InvalidEncoding(
                        "serializable snapshot capture was replayed",
                    ));
                }
                candidate =
                    Some(coordinator.admit_serializable_snapshot(options.read_only, control)?);
                Ok(())
            })?;
            let (participant, committed) = candidate.ok_or(VersionError::InvalidEncoding(
                "serializable snapshot capture was not invoked",
            ))?;
            let mut context = SerializableReadContext {
                persistence: Arc::clone(persistence),
                participant,
                memory: control.memory().clone(),
                safe: false,
            };
            if options.read_only && options.deferrable {
                if !context.wait_for_safe_snapshot(control)? {
                    context.with_graph(control, |graph| graph.rollback(context.id()))?;
                    continue;
                }
                context.safe = true;
            }
            let mark = context.with_graph(control, |graph| graph.write_mark(context.id()))?;
            // The first fixed snapshot belongs to the outer transaction, including when acquired after a SQL savepoint.
            for savepoint in &mut *self.savepoints {
                savepoint.serializable = Some(mark);
                savepoint.committed = Arc::clone(&committed);
            }
            self.committed = committed;
            self.serializable = Some(context.clone());
            return Ok(context);
        }
    }
}

impl SerializableReadContext {
    fn wait_for_safe_snapshot(&self, control: &StorageReadControl) -> VersionResult<bool> {
        loop {
            match self.safe_snapshot(control)? {
                SafeSnapshot::Safe => return Ok(true),
                SafeSnapshot::Unsafe => return Ok(false),
                SafeSnapshot::Pending => {
                    // Neither caller snapshot guards nor provider admission remain held while an overlapping writer finishes.
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}
