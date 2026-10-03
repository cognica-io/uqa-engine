//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Terminating a session after `idle_in_transaction_session_timeout`, `idle_session_timeout` or `transaction_timeout`, as `PostgreSQL` terminates the backend: the session's cancellation token reports the termination to every later check, and the transaction of a session that is idle when its time comes is rolled back at once, which releases its locks.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use uqa_core::{CancellationReason, ScheduledAction};

use super::Engine;

/// The terminations a session has scheduled for its idle period and its transaction.
pub(crate) struct SessionTerminations {
    /// Whether this engine owns the session's idle periods; engines forked to work for a session do not.
    tracks_idle: bool,
    /// How many nested holds of the statement gate this engine has.
    gate_depth: AtomicUsize,
    /// Counts the session's busy periods, so that a termination scheduled for an idle period that has ended does nothing.
    busy_epoch: Arc<AtomicU64>,
    idle: Mutex<Option<ScheduledAction>>,
    /// The termination scheduled for the open transaction, with the start of the transaction it belongs to.
    transaction: Mutex<Option<(i64, ScheduledAction)>>,
}

impl SessionTerminations {
    pub(crate) fn new(tracks_idle: bool) -> Self {
        Self {
            tracks_idle,
            gate_depth: AtomicUsize::new(0),
            busy_epoch: Arc::new(AtomicU64::new(0)),
            idle: Mutex::new(None),
            transaction: Mutex::new(None),
        }
    }

    /// Note one more hold of the statement gate; `true` when it is the outermost, which begins a busy period.
    pub(crate) fn enter_gate(&self) -> bool {
        self.tracks_idle && self.gate_depth.fetch_add(1, Ordering::AcqRel) == 0
    }

    /// Note the release of a hold of the statement gate; `true` when it was the outermost, which begins an idle period.
    pub(crate) fn leave_gate(&self) -> bool {
        self.tracks_idle && self.gate_depth.fetch_sub(1, Ordering::AcqRel) == 1
    }

    /// Whether no operation holds the statement gate, so that the session is idle.
    pub(crate) fn is_idle(&self) -> bool {
        self.gate_depth.load(Ordering::Acquire) == 0
    }

    /// Cancel every scheduled termination, as closing the session does.
    pub(crate) fn disarm(&self) {
        *self.idle.lock() = None;
        *self.transaction.lock() = None;
    }
}

/// What a scheduled termination must still find when its time comes for it to apply.
enum TerminationScope {
    /// The idle period it was scheduled for, which ends when the session is busy again.
    IdlePeriod {
        epoch: Arc<AtomicU64>,
        expected: u64,
    },
    /// The transaction that began at this time (microseconds since the epoch), as long as it stays the session's outermost transaction; a statement outside a transaction block has none, and its termination is canceled when it ends.
    Transaction { started: i64, explicit: bool },
}

/// A timeout of the session in milliseconds, or `None` for `0`.
fn timeout(engine: &Engine, name: &str) -> Option<Duration> {
    let milliseconds = engine.session.setting(name).parse::<u64>().ok()?;
    (milliseconds != 0).then(|| Duration::from_millis(milliseconds))
}

impl Engine {
    /// A busy period begins: the idle termination no longer applies, and a statement outside a transaction block starts the transaction that `transaction_timeout` limits.
    pub(crate) fn session_became_busy(&self) {
        let terminations = &self.runtime.terminations;
        terminations.busy_epoch.fetch_add(1, Ordering::AcqRel);
        *terminations.idle.lock() = None;
        if self.transaction_depth() == 0 {
            self.schedule_transaction_termination(uqa_sql::expr::clock_timestamp_micros());
        }
    }

    /// An idle period begins: a terminated session finishes its termination, and otherwise the idle and transaction terminations that apply to it are scheduled.
    pub(crate) fn session_became_idle(&self) {
        if self.runtime.cancellation.termination().is_some() {
            self.session_terminator_engine()
                .finish_session_termination();
            return;
        }
        let transaction_start = self
            .session
            .transactions
            .lock()
            .first()
            .map(|frame| frame.started_at_micros);
        let (name, reason) = if let Some(started) = transaction_start {
            self.schedule_transaction_termination(started);
            (
                "idle_in_transaction_session_timeout",
                CancellationReason::IdleInTransactionSessionTimeout,
            )
        } else {
            *self.runtime.terminations.transaction.lock() = None;
            (
                "idle_session_timeout",
                CancellationReason::IdleSessionTimeout,
            )
        };
        let Some(after) = timeout(self, name) else {
            return;
        };
        let epoch = Arc::clone(&self.runtime.terminations.busy_epoch);
        let expected = epoch.load(Ordering::Acquire);
        let action =
            self.termination_action(reason, TerminationScope::IdlePeriod { epoch, expected });
        *self.runtime.terminations.idle.lock() = Some(uqa_core::cancel::schedule(after, action));
    }

    /// Schedule the termination of the transaction that began at `started` (microseconds since the epoch) once it outlasts `transaction_timeout`, unless one is already scheduled for it.
    fn schedule_transaction_termination(&self, started: i64) {
        let mut scheduled = self.runtime.terminations.transaction.lock();
        if scheduled
            .as_ref()
            .is_some_and(|(start, _)| *start == started)
        {
            return;
        }
        *scheduled = None;
        let Some(limit) = timeout(self, "transaction_timeout") else {
            return;
        };
        let elapsed = uqa_sql::expr::clock_timestamp_micros().saturating_sub(started);
        let elapsed = Duration::from_micros(u64::try_from(elapsed).unwrap_or(0));
        let explicit = self.transaction_depth() != 0;
        let action = self.termination_action(
            CancellationReason::TransactionTimeout,
            TerminationScope::Transaction { started, explicit },
        );
        *scheduled = Some((
            started,
            uqa_core::cancel::schedule(limit.saturating_sub(elapsed), action),
        ));
    }

    /// The action that terminates the session for `reason`: it terminates the session's token, which ends a running statement, and rolls back the transaction of a session that is idle on a thread of its own. It does nothing once `scope` has ended.
    fn termination_action(
        &self,
        reason: CancellationReason,
        scope: TerminationScope,
    ) -> impl FnOnce() + Send + 'static {
        let token = self.runtime.cancellation.clone();
        let terminator = self.session_terminator_engine();
        move || {
            let applies = match &scope {
                TerminationScope::IdlePeriod { epoch, expected } => {
                    epoch.load(Ordering::Acquire) == *expected
                }
                TerminationScope::Transaction { started, explicit } => {
                    !*explicit
                        || terminator
                            .session
                            .transactions
                            .lock()
                            .first()
                            .is_some_and(|frame| frame.started_at_micros == *started)
                }
            };
            if !applies {
                return;
            }
            token.cancel_with(reason);
            // Without a thread of its own the termination still holds: the session finishes it when its owner next uses it.
            let _ = std::thread::Builder::new()
                .name("uqa-session-termination".into())
                .spawn(move || terminator.terminate_idle_session());
        }
    }

    /// Roll back the transaction of a terminated session that is idle; a session that is busy finishes its own termination when its statement ends.
    fn terminate_idle_session(&self) {
        let Some(_statement) = self.runtime.statement_gate.try_lock() else {
            return;
        };
        self.finish_session_termination();
    }

    /// Roll back every transaction frame of the terminated session and drop what it holds, once, as the backend's exit does.
    fn finish_session_termination(&self) {
        if self
            .session
            .termination_finished
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        while self.transaction_depth() != 0 {
            if self.rollback().is_err() {
                break;
            }
        }
        let _ = self.clear_notification_listener_without_transaction();
        self.release_automatic_statistics_client();
    }

    /// An engine that works for this session on another thread: it shares the session, its storage, its locks and its statement gate, holds a token of its own, and tracks no idle periods.
    fn session_terminator_engine(&self) -> Engine {
        let mut epochs = crate::EpochCoordinator::new();
        epochs.share_published_from(&self.epochs);
        let mut runtime = crate::QueryRuntime::new(
            self.sql_function_depth_limit(),
            self.session.state.client_level(),
        );
        runtime.diagnostics = self.runtime.diagnostics.fork();
        runtime.statement_gate = Arc::clone(&self.runtime.statement_gate);
        runtime.notices = Arc::clone(&self.runtime.notices);
        runtime.notifications = Arc::clone(&self.runtime.notifications);
        runtime.terminations = SessionTerminations::new(false);
        Engine {
            storage: crate::StorageContext::shared_from(&self.storage),
            durable: Arc::clone(&self.durable),
            session: Arc::clone(&self.session),
            extensions: crate::RuntimeExtensions::shared_from(&self.extensions),
            epochs,
            runtime,
            row_locks: Arc::clone(&self.row_locks),
            statistics: Arc::clone(&self.statistics),
            notification_hub: Arc::clone(&self.notification_hub),
            session_id: self.session_id,
            owns_session_registration: false,
            query_table_snapshots: None,
            query_view_snapshots: None,
            query_sql_function_snapshots: None,
            query_catalog_snapshot: None,
            query_transaction_overlay: None,
            query_transaction_origin: None,
        }
    }

    /// The error that terminated this session, if a session timeout did; every later statement reports it, and a server closes the connection after sending it at `FATAL`.
    pub fn session_termination(&self) -> Option<uqa_sql::SQLError> {
        self.runtime
            .cancellation
            .termination()
            .map(|reason| uqa_core::QueryCancelled::new(reason).into())
    }
}
