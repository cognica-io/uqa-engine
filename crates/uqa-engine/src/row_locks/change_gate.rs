//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped row-change snapshot and publication gates.

use super::{
    change_gate_claim, ByteClaim, CrossAttachment, Duration, Instant, RowChangeBaseline,
    RowLockManager, RwLockReadGuard, RwLockWriteGuard, SQLError, CHANGE_GATE_SESSION,
    CHANGE_GATE_WAIT_LIMIT, WAIT_SLICE,
};

pub(crate) struct RowChangeSnapshot<'manager> {
    manager: &'manager RowLockManager,
    cross_claim: Option<ByteClaim>,
    _local: RwLockReadGuard<'manager, ()>,
}

pub(crate) struct RowChangePublication<'manager> {
    manager: &'manager RowLockManager,
    cross_claim: Option<ByteClaim>,
    _local: RwLockWriteGuard<'manager, ()>,
}

impl RowChangeSnapshot<'_> {
    pub(crate) fn baseline(&self) -> Result<RowChangeBaseline, SQLError> {
        let epoch = self.manager.current_change_epoch();
        let cross_sequence = match self.manager.cross.as_ref() {
            Some(CrossAttachment::Active(coordinator)) => {
                coordinator.change_sequence().map_err(SQLError::Internal)?
            }
            _ => 0,
        };
        Ok(RowChangeBaseline {
            epoch,
            cross_sequence,
        })
    }
}

impl Drop for RowChangeSnapshot<'_> {
    fn drop(&mut self) {
        if let (Some(CrossAttachment::Active(coordinator)), Some(claim)) =
            (self.manager.cross.as_ref(), self.cross_claim)
        {
            coordinator.release(CHANGE_GATE_SESSION, &[claim]);
        }
    }
}

impl Drop for RowChangePublication<'_> {
    fn drop(&mut self) {
        if let (Some(CrossAttachment::Active(coordinator)), Some(claim)) =
            (self.manager.cross.as_ref(), self.cross_claim)
        {
            coordinator.release(CHANGE_GATE_SESSION, &[claim]);
        }
    }
}

impl RowLockManager {
    fn acquire_change_gate_claim(
        &self,
        write: bool,
        cancel: &uqa_core::CancellationToken,
        deadline: Instant,
    ) -> Result<Option<ByteClaim>, SQLError> {
        let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() else {
            return Ok(None);
        };
        let claim = change_gate_claim(write);
        loop {
            cancel.check()?;
            if let Ok(()) = coordinator
                .try_claim(CHANGE_GATE_SESSION, &[claim])
                .map_err(SQLError::Internal)?
            {
                return Ok(Some(claim));
            }
            if Instant::now() >= deadline {
                return Err(change_gate_timeout());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Hold the shared commit/snapshot gate while a storage snapshot is pinned and its row-change baseline is captured.
    pub(crate) fn begin_change_snapshot(
        &self,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<RowChangeSnapshot<'_>, SQLError> {
        let deadline = Instant::now() + CHANGE_GATE_WAIT_LIMIT;
        let local = loop {
            cancel.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(change_gate_timeout());
            }
            if let Some(local) = self.change_gate.try_read_for(remaining.min(WAIT_SLICE)) {
                break local;
            }
        };
        let cross_claim = self.acquire_change_gate_claim(false, cancel, deadline)?;
        Ok(RowChangeSnapshot {
            manager: self,
            cross_claim,
            _local: local,
        })
    }

    /// Hold the exclusive commit/snapshot gate from immediately before the backend commit through publication of its row-change metadata.
    pub(crate) fn begin_change_publication(
        &self,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<RowChangePublication<'_>, SQLError> {
        let deadline = Instant::now() + CHANGE_GATE_WAIT_LIMIT;
        let local = loop {
            cancel.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(change_gate_timeout());
            }
            if let Some(local) = self.change_gate.try_write_for(remaining.min(WAIT_SLICE)) {
                break local;
            }
        };
        let cross_claim = self.acquire_change_gate_claim(true, cancel, deadline)?;
        Ok(RowChangePublication {
            manager: self,
            cross_claim,
            _local: local,
        })
    }
}

pub(super) fn change_gate_timeout() -> SQLError {
    SQLError::Routine {
        sqlstate: "55P03".into(),
        message: format!(
            "timed out after {} seconds waiting for cross-process row-change coordination",
            CHANGE_GATE_WAIT_LIMIT.as_secs()
        ),
    }
}
