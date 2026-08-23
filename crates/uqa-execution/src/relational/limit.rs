//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming LIMIT/OFFSET, including ordered `FETCH ... WITH TIES`.

use super::{
    compare_sort_key_values, Batch, ExecResult, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator, SortKey, Value,
};

struct WithTies<'a> {
    keys: Vec<SortKey>,
    evaluator: SharedExpressionEvaluator<'a>,
    boundary: Option<Vec<Value>>,
    finished: bool,
}

pub struct Limit<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    emitted: u64,
    schema: RowSchema,
    with_ties: Option<WithTies<'a>>,
}

impl<'a> Limit<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, offset: u64, limit: Option<u64>) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            offset,
            limit,
            skipped: 0,
            emitted: 0,
            schema,
            with_ties: None,
        }
    }

    /// Build an ordered `FETCH ... WITH TIES` boundary. The caller must pass the complete effective `ORDER BY` key list and a non-null row count.
    pub fn with_ties(
        child: Box<dyn PhysicalOperator + 'a>,
        offset: u64,
        limit: u64,
        mut keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let schema = child.row_schema().clone();
        for key in &mut keys {
            let expression = std::mem::replace(&mut key.expr, ScalarExpr::Literal(Value::Null));
            key.expr = evaluator.bind_type_introspection(expression, &schema);
        }
        Self {
            child,
            offset,
            limit: Some(limit),
            skipped: 0,
            emitted: 0,
            schema,
            with_ties: Some(WithTies {
                keys,
                evaluator,
                boundary: None,
                finished: false,
            }),
        }
    }
}

impl PhysicalOperator for Limit<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.child.output_ordering()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.skipped = 0;
        self.emitted = 0;
        if let Some(with_ties) = self.with_ties.as_mut() {
            with_ties.boundary = None;
            with_ties.finished = false;
        }
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.limit == Some(0)
            || self
                .with_ties
                .as_ref()
                .is_some_and(|with_ties| with_ties.finished)
            || self.with_ties.is_none() && self.limit.is_some_and(|limit| self.emitted >= limit)
        {
            return Ok(None);
        }
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            let mut buf = Vec::new();
            for row in batch.rows {
                if self.skipped < self.offset {
                    self.skipped += 1;
                    continue;
                }
                if let Some(lim) = self.limit {
                    if self.emitted >= lim {
                        let Some(with_ties) = self.with_ties.as_mut() else {
                            return if buf.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)))
                            };
                        };
                        let values = with_ties
                            .keys
                            .iter()
                            .map(|key| {
                                with_ties
                                    .evaluator
                                    .evaluate_physical(&key.expr, &self.schema, &row)
                            })
                            .collect::<ExecResult<Vec<_>>>()?;
                        let boundary = with_ties.boundary.as_ref().ok_or_else(|| {
                            crate::ExecError::Other(
                                "WITH TIES boundary was not captured".to_string(),
                            )
                        })?;
                        if compare_sort_key_values(&with_ties.keys, boundary, &values)
                            != std::cmp::Ordering::Equal
                        {
                            with_ties.finished = true;
                            return if buf.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)))
                            };
                        }
                    } else if self.with_ties.is_some() && self.emitted + 1 == lim {
                        let with_ties = self.with_ties.as_mut().expect("checked above");
                        with_ties.boundary = Some(
                            with_ties
                                .keys
                                .iter()
                                .map(|key| {
                                    with_ties.evaluator.evaluate_physical(
                                        &key.expr,
                                        &self.schema,
                                        &row,
                                    )
                                })
                                .collect::<ExecResult<Vec<_>>>()?,
                        );
                    }
                }
                buf.push(row);
                self.emitted += 1;
            }
            if !buf.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), buf)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}
