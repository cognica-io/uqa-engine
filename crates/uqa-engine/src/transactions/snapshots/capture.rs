//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed selection is admitted by the transaction owner, independently of SQL execution.

use crate::{
    state::{SnapshotCut, SnapshotSources},
    Engine, SQLError, TransactionFrame,
};
use std::time::Duration;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

const WAIT: Duration = Duration::from_millis(10);

#[cfg(test)]
thread_local! {
    static ADMISSION_WAIT: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn observe_admission_wait(sender: std::sync::mpsc::SyncSender<()>) {
    ADMISSION_WAIT.with(|observer| *observer.borrow_mut() = Some(sender));
}

fn admission_waited() {
    #[cfg(test)]
    ADMISSION_WAIT.with(|observer| {
        if let Some(sender) = observer.borrow_mut().take() {
            let _ = sender.try_send(());
        }
    });
}

impl Engine {
    /// Admit a public committed-memory read before selecting its cut. A fresh operation owns an idle generation; a nested operation requires the outer frame's originals. Authorization and transaction failure handling precede selection, and temporary source owners retire after the operation gate closes.
    pub(crate) fn capture_committed_memory_query(
        &self,
        control: &StorageReadControl,
        authorize: impl FnOnce(&Self) -> Result<(), SQLError>,
    ) -> Result<SnapshotCut, SQLError> {
        let mut retiring = None;
        let selected = {
            let nested_operation = self.runtime.statement_gate.owns_current_operation();
            let _statement = self
                .runtime
                .statement_gate
                .lock_with_cancellation(control.cancellation())?;
            let (_execution, nested) = crate::queries::SQLExecutionScope::enter(self);
            self.with_query_transaction_snapshot(
                !nested,
                true,
                |engine| {
                    authorize(engine)?;
                    loop {
                        control.check().map_err(|error| capture_error(&error))?;
                        let Some(frames) = engine.session.transactions.try_lock_for(WAIT) else {
                            admission_waited();
                            continue;
                        };
                        if let Some(frame) = frames.first() {
                            return engine
                                .capture_original_memory(frame, control)
                                .map_err(|error| capture_error(&error));
                        }
                        if nested_operation {
                            return Err(capture_error(&unframed()));
                        }
                        let sources = SnapshotSources::live(
                            &engine.storage.tables,
                            &engine.durable.snapshot(),
                        )
                        .map_err(|error| capture_error(&error))?;
                        let selected = sources.capture(engine.row_locks.catalog_oids(), control);
                        retiring = Some(sources);
                        return selected.map_err(|error| capture_error(&error));
                    }
                },
                std::convert::identity,
            )
        };
        drop(retiring);
        let selected = selected?;
        selected
            .cut
            .check()
            .map_err(|error| capture_error(&error))?;
        Ok(selected)
    }

    fn capture_original_memory(
        &self,
        frame: &TransactionFrame,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SnapshotCut> {
        frame
            .data_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.data.sources.as_ref())
            .ok_or_else(|| {
                StorageBackendError::Other(
                    "memory transaction has no original capture sources".into(),
                )
            })?
            .capture(self.row_locks.catalog_oids(), control)
    }

    /// Select the outer original generation or an idle generation without creating a transaction. Source registration holds transaction admission through the matching identifier cut. Idle source retirement happens after releasing operation admission.
    #[cfg(test)]
    pub(crate) fn capture_committed_memory(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<SnapshotCut> {
        control.check()?;
        if self.storage.backend.is_some() {
            return Err(StorageBackendError::Other(
                "persistent storage uses its provider's committed capture".into(),
            ));
        }
        let (capture, sources) = loop {
            control.check()?;
            let Some(frames) = self.session.transactions.try_lock_for(WAIT) else {
                admission_waited();
                continue;
            };
            if let Some(frame) = frames.first() {
                return self.capture_original_memory(frame, control);
            }
            // Never wait for the statement gate while holding transaction admission: an operation can own that gate while starting its first frame.
            if let Some(_statement) = self.runtime.statement_gate.try_lock_idle() {
                let sources = crate::state::SnapshotSources::live(
                    &self.storage.tables,
                    &self.durable.snapshot(),
                )?;
                let capture = sources.capture(self.row_locks.catalog_oids(), control);
                break (capture, sources);
            }
            if self.runtime.statement_gate.owns_current_operation() {
                return Err(unframed());
            }
            drop(frames);
            admission_waited();
            // A new frame may now provide originals even while its statement remains busy. Reconsider transaction admission instead of waiting for that statement to finish.
            std::thread::park_timeout(WAIT);
        };
        // The independently selected sources preserve their admitted values here. No SQL gate or transaction lock is held during retirement, and no undo handle escapes to the returned cut.
        drop(sources);
        let capture = capture?;
        capture.cut.check()?;
        Ok(capture)
    }
}

fn unframed() -> StorageBackendError {
    StorageBackendError::Other(
        "an unframed operation cannot capture its own unfinished memory writes".into(),
    )
}

fn capture_error(error: &StorageBackendError) -> SQLError {
    uqa_execution::storage_errors::storage_error("capture committed memory", error)
}
