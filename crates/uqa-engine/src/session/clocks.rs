//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL message and transaction timestamps owned by one logical session.

use std::sync::atomic::Ordering;

use crate::Engine;

impl Engine {
    pub(crate) fn statement_timestamp_micros(&self) -> i64 {
        self.statement_clock_micros()
            .unwrap_or_else(uqa_sql::expr::clock_timestamp_micros)
    }

    pub(crate) fn transaction_timestamp_micros(&self) -> i64 {
        self.session_transaction_clock_micros()
            .unwrap_or_else(uqa_sql::expr::clock_timestamp_micros)
    }

    /// The transaction start the session's statements read: the outermost transaction's start, or the current SQL message's start outside a transaction. `None` while the session runs neither, as during a direct API call outside any transaction on an engine that opens none for it, where each evaluation reads the wall clock as `now()` does.
    pub(crate) fn session_transaction_clock_micros(&self) -> Option<i64> {
        self.session
            .transactions
            .lock()
            .first()
            .map(|frame| frame.started_at_micros)
            .or_else(|| self.statement_clock_micros())
    }

    fn statement_clock_micros(&self) -> Option<i64> {
        match self
            .session
            .statement_started_at_micros
            .load(Ordering::Relaxed)
        {
            0 => None,
            micros => Some(micros),
        }
    }
}
