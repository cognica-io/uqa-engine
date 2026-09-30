//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Running aggregates over frames that start at the partition start and end at the current row. `ROWS` ends at the row itself; `RANGE` and `GROUPS` end at the last peer of the row, so peers share one value.

use crate::aggregation::AggregateAccumulator;
use uqa_core::Value;
use uqa_sql::SQLError;

/// The partition rows a running aggregate reads ahead of the current row.
pub(super) trait WindowRows {
    fn order_key(&mut self, position: u64) -> Result<Vec<Value>, SQLError>;
    fn argument(&mut self, position: u64) -> Result<Value, SQLError>;
}

pub(super) struct RunningWindowAggregate {
    accumulator: AggregateAccumulator,
    includes_peers: bool,
    /// Rows before this partition position have been observed.
    observed_through: u64,
    value: Value,
}

impl RunningWindowAggregate {
    pub(super) fn new(accumulator: AggregateAccumulator, includes_peers: bool) -> Self {
        Self {
            accumulator,
            includes_peers,
            observed_through: 0,
            value: Value::Null,
        }
    }

    /// The aggregate for the row at `index` with order key `key`. Rows are visited in partition order; reaching a row past the observed prefix observes it and, when peers count, every following peer. A following row's argument is evaluated only once it is known to be a peer.
    pub(super) fn value_at(
        &mut self,
        index: u64,
        key: &[Value],
        rows: &mut dyn WindowRows,
        row_count: u64,
        finish: impl FnOnce(&AggregateAccumulator) -> Result<Value, SQLError>,
    ) -> Result<Value, SQLError> {
        if index < self.observed_through {
            return Ok(self.value.clone());
        }
        self.accumulator.observe(&rows.argument(index)?)?;
        let mut next = index + 1;
        while self.includes_peers && next < row_count && rows.order_key(next)? == key {
            self.accumulator.observe(&rows.argument(next)?)?;
            next += 1;
        }
        self.observed_through = next;
        self.value = finish(&self.accumulator)?;
        Ok(self.value.clone())
    }
}

/// The rows of one sorted partition, evaluated with the window call's arguments and ORDER BY keys.
pub(super) struct PartitionRows<'a> {
    pub(super) partition: &'a mut crate::IndexedSpill,
    pub(super) name: &'a str,
    pub(super) slot: &'a super::WindowSlot,
    pub(super) schema: &'a crate::RowSchema,
    pub(super) params: &'a [uqa_sql::SQLParam],
    pub(super) eval_hook: &'a dyn uqa_sql::expr::EngineHook,
    pub(super) subquery_runner: &'a dyn crate::ScalarSubqueryRunner,
}

impl WindowRows for PartitionRows<'_> {
    fn order_key(&mut self, position: u64) -> Result<Vec<Value>, SQLError> {
        let row = self
            .partition
            .get(position)
            .map_err(super::exec_to_sql_error)?;
        super::evaluate_order_key(
            &self.slot.spec.order_by,
            self.schema,
            &row,
            self.params,
            self.eval_hook,
            self.subquery_runner,
        )
    }

    fn argument(&mut self, position: u64) -> Result<Value, SQLError> {
        let row = self
            .partition
            .get(position)
            .map_err(super::exec_to_sql_error)?;
        super::window_aggregate_argument(
            self.name,
            &self.slot.args,
            self.schema,
            &row,
            self.params,
            self.eval_hook,
            self.subquery_runner,
        )
    }
}
