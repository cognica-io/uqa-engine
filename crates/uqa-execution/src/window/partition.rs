//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Random access to the rows of one sorted window partition: row reads, expression evaluation on a row, and the peer comparison of the window's `ORDER BY` keys that `are_peers` makes.

use crate::spill::BufferedIndexedSpill;
use crate::{
    PhysicalRow, RowSchema, RowSchemaExecution, ScalarEvalContext, ScalarOrder,
    ScalarSubqueryRunner,
};
use std::cmp::Ordering;
use uqa_core::memory::ProductionControl;
use uqa_core::Value;
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

/// Rows read most recently; the frame and peer-group pointers revisit a few neighboring positions for every current row.
const CACHED_ROWS: usize = 8;

struct CachedRow {
    position: i64,
    row: PhysicalRow,
    order_key: Option<Vec<Value>>,
}

pub(super) struct PartitionRows<'a> {
    rows: &'a mut BufferedIndexedSpill,
    schema: RowSchema,
    len: i64,
    order_by: &'a [ScalarOrder],
    params: &'a [SQLParam],
    hook: &'a dyn uqa_sql::expr::EngineHook,
    subqueries: &'a dyn ScalarSubqueryRunner,
    cache: Vec<CachedRow>,
    next_victim: usize,
}

impl<'a> PartitionRows<'a> {
    pub(super) fn new(
        rows: &'a mut BufferedIndexedSpill,
        order_by: &'a [ScalarOrder],
        params: &'a [SQLParam],
        hook: &'a dyn uqa_sql::expr::EngineHook,
        subqueries: &'a dyn ScalarSubqueryRunner,
    ) -> Result<Self, SQLError> {
        let len = i64::try_from(rows.len())
            .map_err(|_| SQLError::Internal("window partition row count exceeds i64".into()))?;
        Ok(Self {
            schema: rows.row_schema().clone(),
            rows,
            len,
            order_by,
            params,
            hook,
            subqueries,
            cache: Vec::with_capacity(CACHED_ROWS),
            next_victim: 0,
        })
    }

    pub(super) const fn len(&self) -> i64 {
        self.len
    }

    pub(super) const fn is_ordered(&self) -> bool {
        !self.order_by.is_empty()
    }

    fn cached(&mut self, position: i64) -> Result<usize, SQLError> {
        if let Some(index) = self
            .cache
            .iter()
            .position(|entry| entry.position == position)
        {
            return Ok(index);
        }
        let index = u64::try_from(position).map_err(|_| out_of_partition(position))?;
        let row = self.rows.get(index).map_err(super::exec_to_sql_error)?;
        let entry = CachedRow {
            position,
            row,
            order_key: None,
        };
        if self.cache.len() < CACHED_ROWS {
            self.cache.push(entry);
            Ok(self.cache.len() - 1)
        } else {
            let victim = self.next_victim;
            self.next_victim = (victim + 1) % CACHED_ROWS;
            self.cache[victim] = entry;
            Ok(victim)
        }
    }

    pub(super) fn row(&mut self, position: i64) -> Result<PhysicalRow, SQLError> {
        let index = self.cached(position)?;
        Ok(self.cache[index].row.clone())
    }

    /// Run `body` with an evaluation context for the row at `position`.
    pub(super) fn with_context<T>(
        &mut self,
        position: i64,
        body: impl FnOnce(&ScalarEvalContext<'_>) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let row = self.row(position)?;
        let view = self.schema.view(&row);
        let context = ScalarEvalContext::from_row_lookup(&view, self.params)
            .with_function_hook(self.hook)
            .with_subquery_runner(self.subqueries)
            .with_physical_outer_row(&self.schema, &row);
        body(&context)
    }

    /// The catalog that renders enum labels in finalized aggregates.
    pub(super) fn enum_labels(&self) -> Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog> {
        self.hook.enum_labels()
    }

    /// Evaluate an expression on the row at `position`.
    pub(super) fn evaluate(
        &mut self,
        expression: &ScalarExpr,
        position: i64,
    ) -> Result<Value, SQLError> {
        let row = self.row(position)?;
        super::evaluate_on_row(
            expression,
            &self.schema,
            &row,
            self.params,
            self.hook,
            self.subqueries,
        )
    }

    fn order_key(&mut self, position: i64) -> Result<Vec<Value>, SQLError> {
        let index = self.cached(position)?;
        if let Some(key) = &self.cache[index].order_key {
            return Ok(key.clone());
        }
        let row = self.cache[index].row.clone();
        let key = self
            .order_by
            .iter()
            .map(|order| {
                super::evaluate_on_row(
                    &order.expr,
                    &self.schema,
                    &row,
                    self.params,
                    self.hook,
                    self.subqueries,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.cache[index].order_key = Some(key.clone());
        Ok(key)
    }

    /// The value of the first `ORDER BY` key, which a `RANGE` frame with an offset measures.
    pub(super) fn sort_value(&mut self, position: i64) -> Result<Value, SQLError> {
        Ok(self
            .order_key(position)?
            .into_iter()
            .next()
            .unwrap_or(Value::Null))
    }

    /// `are_peers`: rows whose `ORDER BY` keys are equal under the operators that sort them, NULL matching NULL. Without `ORDER BY` every row is a peer of every other.
    pub(super) fn are_peers(&mut self, left: i64, right: i64) -> Result<bool, SQLError> {
        if self.order_by.is_empty() || left == right {
            return Ok(true);
        }
        let left = self.order_key(left)?;
        let right = self.order_key(right)?;
        for (left, right) in left.iter().zip(&right) {
            let ordering = uqa_sql::expr::compare_typed_values_with_control(
                left,
                right,
                &ProductionControl::uncontrolled(),
            )?;
            if ordering != Ordering::Equal {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn out_of_partition(position: i64) -> SQLError {
    SQLError::Internal(format!("window row {position} lies outside its partition"))
}
