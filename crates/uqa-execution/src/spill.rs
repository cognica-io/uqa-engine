//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory spill buffer for blocking operators (`Sort`,
//! `HashAggregate`, `Window`). The Python reference spills to a tmp
//! file once an in-memory threshold is exceeded; the Rust port mirrors
//! the API but currently keeps every batch in memory. The on-disk
//! tier is gated on the `arrow-rs` integration and tracked as a
//! follow-up.

use crate::batch::Batch;

/// Append-only batch buffer. Operators push input batches in,
/// optionally trigger [`Self::spill_if_over_budget`] between phases,
/// then drain the buffer with [`Self::drain`] in input order.
pub struct SpillBuffer {
    batches: Vec<Batch>,
    rows: usize,
    /// Soft row budget. Set to `usize::MAX` to disable.
    budget: usize,
}

impl SpillBuffer {
    pub fn new(budget: usize) -> Self {
        Self {
            batches: Vec::new(),
            rows: 0,
            budget,
        }
    }

    pub fn unbounded() -> Self {
        Self::new(usize::MAX)
    }

    pub fn push(&mut self, batch: Batch) {
        self.rows = self.rows.saturating_add(batch.rows.len());
        self.batches.push(batch);
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn over_budget(&self) -> bool {
        self.rows > self.budget
    }

    /// Hook called by operators between phases. The current
    /// implementation is a no-op (in-memory tier only); the follow-up
    /// implementation will flush full batches to a temp file once
    /// over the budget and read them back during [`Self::drain`].
    pub fn spill_if_over_budget(&mut self) {
        let _ = self.over_budget();
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, Batch> {
        self.rows = 0;
        self.batches.drain(..)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{Batch, RowSchema};
    use std::collections::BTreeMap;
    use uqa_core::Value;

    fn dummy_batch(n: usize) -> Batch {
        let schema = RowSchema::new(vec!["x".into()]);
        let mut rows = Vec::new();
        for i in 0..n {
            let mut row: BTreeMap<String, Value> = BTreeMap::new();
            row.insert("x".into(), Value::Int(i as i64));
            rows.push(row);
        }
        Batch::new(schema, rows)
    }

    #[test]
    fn budget_flips_over() {
        let mut buf = SpillBuffer::new(5);
        buf.push(dummy_batch(3));
        assert!(!buf.over_budget());
        buf.push(dummy_batch(4));
        assert!(buf.over_budget());
        let drained: Vec<_> = buf.drain().collect();
        assert_eq!(drained.len(), 2);
    }
}
