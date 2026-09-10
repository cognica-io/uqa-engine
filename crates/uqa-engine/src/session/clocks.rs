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
        match self
            .session
            .statement_started_at_micros
            .load(Ordering::Relaxed)
        {
            0 => uqa_sql::expr::clock_timestamp_micros(),
            micros => micros,
        }
    }

    pub(crate) fn transaction_timestamp_micros(&self) -> i64 {
        self.session.transactions.lock().first().map_or_else(
            || self.statement_timestamp_micros(),
            |frame| frame.started_at_micros,
        )
    }
}
