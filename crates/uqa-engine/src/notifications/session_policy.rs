//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the host's notification transport policy with its SQL session.

use std::sync::atomic::Ordering;
use uqa_sql::SQLError;

use crate::Engine;

impl Engine {
    /// Require independently owned subscriptions instead of SQL `LISTEN`/`UNLISTEN` for this session and subsequently created sibling sessions. Stateless SQL hosts must enable this before serving requests. `NOTIFY` and `pg_notify` retain their transactional behavior.
    ///
    /// This is irreversible for the session and survives rollback, DISCARD and nested execution. Enabling it fails with SQLSTATE 55000 during SQL execution, inside a transaction or while SQL channels are registered. Repeated calls after successful configuration are harmless. Independently owned subscription handles are unaffected.
    pub fn require_notification_subscriptions(&self) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        if self.notification_subscriptions_required() {
            return Ok(());
        }
        if self.runtime.sql_execution_depth.load(Ordering::Relaxed) != 0
            || !self.session.transactions.lock().is_empty()
            || !self.session.state.read().listened_channels.is_empty()
        {
            return Err(SQLError::Routine {
                sqlstate: "55000".into(),
                message: "notification subscription policy requires an idle session without SQL listeners".into(),
            });
        }
        self.session
            .notification_subscriptions_required
            .store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn notification_subscriptions_required(&self) -> bool {
        self.session
            .notification_subscriptions_required
            .load(Ordering::Acquire)
    }
}
