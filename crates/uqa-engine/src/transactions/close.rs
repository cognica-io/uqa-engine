//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session closure owns rollback and a separately completed temporary-catalog cleanup transaction.

use super::{Engine, SQLError, TransactionScope};
use std::sync::atomic::{AtomicU8, Ordering};

#[repr(u8)]
enum ClosureState {
    Open,
    Closing,
    Closed,
}

pub(crate) struct SessionClosure(AtomicU8);

impl Default for SessionClosure {
    fn default() -> Self {
        Self(AtomicU8::new(ClosureState::Open as u8))
    }
}

impl SessionClosure {
    pub(crate) fn is_open(&self) -> bool {
        self.0.load(Ordering::Acquire) == ClosureState::Open as u8
    }

    /// The statement gate serializes transitions. Automatic termination starts closure once; after a failed attempt only the explicit owner may resolve its receipt and retry.
    fn begin(&self, explicit: bool) -> bool {
        match self.0.load(Ordering::Acquire) {
            state if state == ClosureState::Closed as u8 => false,
            state if state == ClosureState::Closing as u8 && !explicit => false,
            _ => {
                self.0.store(ClosureState::Closing as u8, Ordering::Release);
                true
            }
        }
    }

    fn finish(&self) {
        self.0.store(ClosureState::Closed as u8, Ordering::Release);
    }
}

impl Engine {
    /// Roll back open transactions, remove this session's temporary objects and their dependents, and release its runtime resources. A failed completion retains the session and its transaction receipt for retry. Successful closure is idempotent; subsequent query use is unsupported.
    pub fn close(&self) -> Result<(), SQLError> {
        self.close_session(true)
    }

    pub(crate) fn close_terminated_session(&self) -> Result<(), SQLError> {
        self.close_session(false)
    }

    fn close_session(&self, explicit: bool) -> Result<(), SQLError> {
        let _statement = self.lock_statement_gate();
        let admitted = self.session.closure.begin(explicit);
        self.runtime.terminations.disarm();
        if !admitted {
            return Ok(());
        }
        self.runtime
            .cancellation
            .with_deferred_cancellation(|| self.finish_session_close())
    }

    fn finish_session_close(&self) -> Result<(), SQLError> {
        while self.transaction_depth() != 0 {
            self.rollback()?;
        }
        self.session.portals.lock().clear();
        if self.temporary_namespace_allocated() {
            let scope = TransactionScope::begin_cleanup(self)?;
            self.run_transaction_scope(
                scope,
                Self::discard_temporary_objects,
                std::convert::identity,
            )?;
        }
        self.clear_notification_listener_without_transaction()?;
        if let Some(cache) = self.session.regex_cache.lock().take() {
            cache.clear();
        }
        self.row_locks.close_temporary_roles(self.session_id);
        self.release_automatic_statistics_client();
        self.session.closure.finish();
        Ok(())
    }
}
