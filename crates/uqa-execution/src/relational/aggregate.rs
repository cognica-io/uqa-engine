//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Blocking hash/sort aggregation and aggregate folds.

use super::{
    compare_values, eval_scalar, Batch, DefaultExpressionEvaluator, ExecError, ExecResult,
    PhysicalOperator, ResultRow, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr, SortKey,
    Value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Count,
    CountStar,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Debug, Clone)]
pub struct AggregateSpec {
    pub kind: AggregateKind,
    /// Argument to the aggregate. Ignored for `CountStar`.
    pub arg: Option<ScalarExpr>,
    /// Output column alias.
    pub alias: String,
    /// `COUNT(DISTINCT x)` / `SUM(DISTINCT x)` / etc.
    pub distinct: bool,
}

/// Blocking group-by + aggregate. Pulls every row from the child
/// during `open`, hashes each row by its group key, and folds the
/// aggregates over each group's row set. Groups are emitted in the
/// order they were first observed.
pub trait AggregateExecutor: Send {
    /// Consume one child batch. Implementations that need a blocking input must
    /// enforce their own byte budget here; the physical operator never creates
    /// an unbounded intermediate row vector.
    fn consume(&mut self, batch: Batch) -> ExecResult<()>;

    /// Finalize all groups into a byte-bounded, disk-backed output stream.
    /// The row-oriented SQL API may materialize that stream at its public API
    /// boundary, but physical operators must not create an unbounded result
    /// vector first.
    fn finish(&mut self) -> ExecResult<crate::spill::SpillBuffer>;
}

pub struct HashAggregate<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    group_keys: Vec<(String, ScalarExpr)>,
    aggregates: Vec<AggregateSpec>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    executor: Option<Box<dyn AggregateExecutor + 'a>>,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
}

impl HashAggregate<'static> {
    const DEFAULT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

    pub fn new(
        child: Box<dyn PhysicalOperator>,
        group_keys: Vec<(String, ScalarExpr)>,
        aggregates: Vec<AggregateSpec>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::new_with_work_mem(
            child,
            group_keys,
            aggregates,
            params,
            Self::DEFAULT_WORK_MEM_BYTES,
        )
    }

    pub fn new_with_work_mem(
        child: Box<dyn PhysicalOperator>,
        group_keys: Vec<(String, ScalarExpr)>,
        aggregates: Vec<AggregateSpec>,
        params: Vec<SQLParam>,
        work_mem_bytes: usize,
    ) -> Self {
        let mut cols: Vec<String> = group_keys.iter().map(|(n, _)| n.clone()).collect();
        for a in &aggregates {
            cols.push(a.alias.clone());
        }
        let schema = RowSchema::new(cols);
        Self {
            child,
            group_keys,
            aggregates,
            params,
            schema,
            executor: None,
            work_mem_bytes,
            output: None,
            output_spilled: false,
        }
    }
}

impl<'a> HashAggregate<'a> {
    /// Construct a physical aggregate backed by the engine's full aggregate
    /// registry. Input is delivered incrementally through
    /// [`AggregateExecutor::consume`].
    pub fn with_executor(
        child: Box<dyn PhysicalOperator + 'a>,
        output_schema: Vec<String>,
        executor: Box<dyn AggregateExecutor + 'a>,
    ) -> Self {
        Self {
            child,
            group_keys: Vec::new(),
            aggregates: Vec::new(),
            params: Vec::new(),
            schema: RowSchema::new(output_schema),
            executor: Some(executor),
            work_mem_bytes: 0,
            output: None,
            output_spilled: false,
        }
    }

    /// Whether final aggregate rows exceeded their output budget and were
    /// written to disk during the current/most recent invocation.
    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }
}

struct GroupState {
    /// Folded aggregate state, one slot per `aggregates` entry.
    folds: Vec<AggFold>,
    /// Group key values, captured on first row.
    key_values: Vec<Value>,
}

pub(super) struct AggFold {
    pub(super) count: u64,
    sum: Option<f64>,
    min: Option<Value>,
    max: Option<Value>,
    distinct: crate::distinct::SeenKeySet,
}

impl AggFold {
    pub(super) fn new(work_mem_bytes: usize) -> Self {
        Self {
            count: 0,
            sum: None,
            min: None,
            max: None,
            distinct: crate::distinct::SeenKeySet::new(work_mem_bytes, None),
        }
    }
}

pub(super) fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

fn fold_into(
    state: &mut AggFold,
    spec: &AggregateSpec,
    row: &ResultRow,
    params: &[SQLParam],
) -> ExecResult<()> {
    match spec.kind {
        AggregateKind::CountStar => {
            state.count = state
                .count
                .checked_add(1)
                .ok_or_else(|| ExecError::Other("aggregate count overflow".into()))?;
        }
        _ => {
            let arg = spec.arg.as_ref().ok_or_else(|| {
                ExecError::Other(format!(
                    "aggregate {:?} requires an argument expression",
                    spec.kind
                ))
            })?;
            let ctx = ScalarEvalContext::new(Some(row), params);
            let v = eval_scalar(arg, &ctx)?;
            if matches!(v, Value::Null) {
                return Ok(());
            }
            if spec.distinct {
                let key = crate::distinct::encode_key(std::slice::from_ref(&v))?;
                if !state.distinct.insert(key)? {
                    return Ok(());
                }
            }
            match spec.kind {
                AggregateKind::Count => {
                    state.count = state
                        .count
                        .checked_add(1)
                        .ok_or_else(|| ExecError::Other("aggregate count overflow".into()))?;
                }
                AggregateKind::Sum | AggregateKind::Avg => {
                    let f = value_to_f64(&v).ok_or_else(|| {
                        ExecError::Other(format!("non-numeric input to SUM/AVG: {v:?}"))
                    })?;
                    state.sum = Some(state.sum.unwrap_or(0.0) + f);
                    state.count = state
                        .count
                        .checked_add(1)
                        .ok_or_else(|| ExecError::Other("aggregate count overflow".into()))?;
                }
                AggregateKind::Min => {
                    state.min = Some(match state.min.take() {
                        None => v,
                        Some(prev) => {
                            if compare_values(&v, &prev) == std::cmp::Ordering::Less {
                                v
                            } else {
                                prev
                            }
                        }
                    });
                }
                AggregateKind::Max => {
                    state.max = Some(match state.max.take() {
                        None => v,
                        Some(prev) => {
                            if compare_values(&v, &prev) == std::cmp::Ordering::Greater {
                                v
                            } else {
                                prev
                            }
                        }
                    });
                }
                AggregateKind::CountStar => {
                    return Err(ExecError::Other(
                        "COUNT(*) reached argument aggregate evaluation".into(),
                    ))
                }
            }
        }
    }
    Ok(())
}

fn finalise_builtin_group(
    state: GroupState,
    group_keys: &[(String, ScalarExpr)],
    aggregates: &[AggregateSpec],
) -> ExecResult<ResultRow> {
    let mut output = ResultRow::new();
    for (index, (alias, _)) in group_keys.iter().enumerate() {
        output.insert(alias.clone(), state.key_values[index].clone());
    }
    for (index, spec) in aggregates.iter().enumerate() {
        output.insert(
            spec.alias.clone(),
            finalise_fold(&state.folds[index], spec)?,
        );
    }
    Ok(output)
}

pub(super) fn finalise_fold(state: &AggFold, spec: &AggregateSpec) -> ExecResult<Value> {
    Ok(match spec.kind {
        AggregateKind::Count | AggregateKind::CountStar => Value::Int(
            i64::try_from(state.count)
                .map_err(|_| ExecError::Other("aggregate count exceeds BIGINT".into()))?,
        ),
        AggregateKind::Sum => state.sum.map(Value::Float).unwrap_or(Value::Null),
        AggregateKind::Avg => match (state.sum, state.count) {
            (Some(s), c) if c > 0 => Value::Float(s / c as f64),
            _ => Value::Null,
        },
        AggregateKind::Min => state.min.clone().unwrap_or(Value::Null),
        AggregateKind::Max => state.max.clone().unwrap_or(Value::Null),
    })
}

impl PhysicalOperator for HashAggregate<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()?;
        self.output_spilled = false;
        if let Some(executor) = self.executor.as_mut() {
            while let Some(batch) = self.child.next()? {
                executor.consume(batch)?;
            }
            let mut output = executor.finish()?;
            self.output_spilled = output.has_spilled();
            self.output = Some(output.drain()?);
            return Ok(());
        }
        let phase_budget = (self.work_mem_bytes / 3).max(1);
        let mut input = crate::spill::SpillBuffer::new(phase_budget);
        while let Some(batch) = self.child.next()? {
            input.push(batch)?;
        }
        let scan: Box<dyn PhysicalOperator> = Box::new(crate::spill_scan::SpillScan::new(
            self.child.schema().to_vec(),
            input,
        ));
        let keys = self
            .group_keys
            .iter()
            .map(|(_, expression)| SortKey {
                expr: expression.clone(),
                descending: false,
                nulls_first: None,
            })
            .collect();
        let evaluator = DefaultExpressionEvaluator::shared(self.params.clone());
        let mut sorted =
            crate::external_sort::ExternalSort::new(scan, keys, evaluator, None, phase_budget);
        sorted.open()?;
        let fold_budget = (phase_budget / self.aggregates.len().max(1)).max(1);
        let mut current_key: Option<Vec<Value>> = None;
        let mut current_state: Option<GroupState> = None;
        let mut output = crate::spill::SpillBuffer::new(phase_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        let execution = (|| -> ExecResult<()> {
            while let Some(batch) = sorted.next()? {
                for row in batch.rows {
                    let ctx = ScalarEvalContext::new(Some(&row), &self.params);
                    let key_values = self
                        .group_keys
                        .iter()
                        .map(|(_, expression)| eval_scalar(expression, &ctx))
                        .collect::<Result<Vec<_>, _>>()?;
                    if current_key
                        .as_ref()
                        .is_some_and(|current| current != &key_values)
                    {
                        pending.push(finalise_builtin_group(
                            current_state.take().ok_or_else(|| {
                                ExecError::Other("active aggregate group has no state".into())
                            })?,
                            &self.group_keys,
                            &self.aggregates,
                        )?);
                        if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
                            output.push(Batch::new(
                                self.schema.clone(),
                                std::mem::take(&mut pending),
                            ))?;
                            pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
                        }
                        current_key = None;
                    }
                    if current_key.is_none() {
                        current_key = Some(key_values.clone());
                        current_state = Some(GroupState {
                            folds: (0..self.aggregates.len())
                                .map(|_| AggFold::new(fold_budget))
                                .collect(),
                            key_values,
                        });
                    }
                    let state = current_state.as_mut().ok_or_else(|| {
                        ExecError::Other("aggregate group state was not initialized".into())
                    })?;
                    for (index, spec) in self.aggregates.iter().enumerate() {
                        fold_into(&mut state.folds[index], spec, &row, &self.params)?;
                    }
                }
            }
            Ok(())
        })();
        let close = sorted.close();
        crate::physical::with_cleanup(execution, close, "close aggregate sort after failure")?;

        if let Some(state) = current_state.take() {
            pending.push(finalise_builtin_group(
                state,
                &self.group_keys,
                &self.aggregates,
            )?);
        } else if self.group_keys.is_empty() {
            let state = GroupState {
                folds: (0..self.aggregates.len())
                    .map(|_| AggFold::new(fold_budget))
                    .collect(),
                key_values: Vec::new(),
            };
            pending.push(finalise_builtin_group(
                state,
                &self.group_keys,
                &self.aggregates,
            )?);
        }
        if !pending.is_empty() {
            output.push(Batch::new(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(output) = self.output.as_mut() else {
            return Ok(None);
        };
        output.next().transpose()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Window
// -------------------------------------------------------------------------
