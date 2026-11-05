//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed selection is admitted by the transaction owner, independently of SQL execution.

use crate::{state::SnapshotCut, Engine};
use std::time::Duration;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

const WAIT: Duration = Duration::from_millis(10);

thread_local! {
    static ADMISSION_WAIT: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn observe_admission_wait(sender: std::sync::mpsc::SyncSender<()>) {
    ADMISSION_WAIT.with(|observer| *observer.borrow_mut() = Some(sender));
}

fn admission_waited() {
    ADMISSION_WAIT.with(|observer| {
        if let Some(sender) = observer.borrow_mut().take() {
            let _ = sender.try_send(());
        }
    });
}

impl Engine {
    /// Select the outer original generation or an idle generation without creating a transaction. Source registration holds transaction admission through the matching identifier cut. Idle source retirement happens after releasing operation admission.
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
                let sources = frame
                    .data_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.data.sources.as_ref())
                    .ok_or_else(|| {
                        StorageBackendError::Other(
                            "memory transaction has no original capture sources".into(),
                        )
                    })?;
                return sources.capture(self.row_locks.catalog_oids(), control);
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
                return Err(StorageBackendError::Other(
                    "an unframed operation cannot capture its own unfinished memory writes".into(),
                ));
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
