//! Byte-bounded window execution.

use super::{
    compare_values, eval_scalar, value_to_f64, Batch, DefaultExpressionEvaluator, ExecError,
    ExecResult, PhysicalOperator, ResultRow, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr,
    SortKey, Value,
};

#[derive(Debug, Clone)]
pub enum WindowKind {
    RowNumber,
    Rank,
    DenseRank,
    Lag(ScalarExpr, i64),
    Lead(ScalarExpr, i64),
    Ntile(i64),
    AggSum(ScalarExpr),
    AggCount(Option<ScalarExpr>),
    AggAvg(ScalarExpr),
    AggMin(ScalarExpr),
    AggMax(ScalarExpr),
}

#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub partition_by: Vec<ScalarExpr>,
    pub order_by: Vec<SortKey>,
}

/// Byte-bounded window operator. Input sorting, random-access partitions, and
/// output rows use disk-backed buffers; only fixed-size batches are decoded at
/// the Volcano boundary.
pub trait WindowExecutor: Send {
    /// Consume one child batch without materializing the complete input in the
    /// physical operator.
    fn consume(&mut self, batch: Batch) -> ExecResult<()>;

    /// Finalize window columns into a byte-bounded, disk-backed output stream.
    fn finish(&mut self) -> ExecResult<crate::spill::SpillBuffer>;
}

pub struct Window<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    spec: WindowSpec,
    functions: Vec<(String, WindowKind)>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    executor: Option<Box<dyn WindowExecutor + 'a>>,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
}

impl Window<'static> {
    const DEFAULT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

    pub fn new(
        child: Box<dyn PhysicalOperator>,
        spec: WindowSpec,
        functions: Vec<(String, WindowKind)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::new_with_work_mem(child, spec, functions, params, Self::DEFAULT_WORK_MEM_BYTES)
    }

    pub fn new_with_work_mem(
        child: Box<dyn PhysicalOperator>,
        spec: WindowSpec,
        functions: Vec<(String, WindowKind)>,
        params: Vec<SQLParam>,
        work_mem_bytes: usize,
    ) -> Self {
        let mut cols = child.schema().to_vec();
        for (name, _) in &functions {
            if !cols.contains(name) {
                cols.push(name.clone());
            }
        }
        let schema = RowSchema::new(cols);
        Self {
            child,
            spec,
            functions,
            params,
            schema,
            executor: None,
            work_mem_bytes,
            output: None,
            output_spilled: false,
        }
    }
}

impl<'a> Window<'a> {
    /// Construct a physical window operator backed by the engine's complete
    /// frame and function implementation.
    pub fn with_executor(
        child: Box<dyn PhysicalOperator + 'a>,
        output_schema: Vec<String>,
        executor: Box<dyn WindowExecutor + 'a>,
    ) -> Self {
        Self {
            child,
            spec: WindowSpec {
                partition_by: Vec::new(),
                order_by: Vec::new(),
            },
            functions: Vec::new(),
            params: Vec::new(),
            schema: RowSchema::new(output_schema),
            executor: Some(executor),
            work_mem_bytes: 0,
            output: None,
            output_spilled: false,
        }
    }

    /// Whether final window rows exceeded their output budget and were written
    /// to disk during the current/most recent invocation.
    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }
}

fn builtin_window_order_key(
    row: &ResultRow,
    spec: &WindowSpec,
    params: &[SQLParam],
) -> ExecResult<Vec<Value>> {
    let context = ScalarEvalContext::new(Some(row), params);
    spec.order_by
        .iter()
        .map(|key| Ok(eval_scalar(&key.expr, &context)?))
        .collect()
}

fn builtin_window_partition_value(
    kind: &WindowKind,
    partition: &mut crate::spill::IndexedSpill,
    params: &[SQLParam],
) -> ExecResult<Option<Value>> {
    let mut count = 0_i64;
    let mut sum = 0.0_f64;
    let mut min = None;
    let mut max = None;
    let expression = match kind {
        WindowKind::AggSum(expression)
        | WindowKind::AggAvg(expression)
        | WindowKind::AggMin(expression)
        | WindowKind::AggMax(expression) => Some(expression),
        WindowKind::AggCount(expression) => expression.as_ref(),
        _ => return Ok(None),
    };
    for index in 0..partition.len() {
        let row = partition.get(index)?;
        let value = match expression {
            Some(expression) => {
                eval_scalar(expression, &ScalarEvalContext::new(Some(&row), params))?
            }
            None => Value::Int(1),
        };
        if matches!(value, Value::Null) {
            continue;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("window aggregate row count overflow".into()))?;
        match kind {
            WindowKind::AggSum(_) | WindowKind::AggAvg(_) => {
                let number = value_to_f64(&value).ok_or_else(|| {
                    ExecError::Other(format!("non-numeric window aggregate input: {value:?}"))
                })?;
                sum += number;
            }
            WindowKind::AggMin(_) => {
                min = Some(match min.take() {
                    Some(previous) if compare_values(&previous, &value).is_le() => previous,
                    _ => value,
                });
            }
            WindowKind::AggMax(_) => {
                max = Some(match max.take() {
                    Some(previous) if compare_values(&previous, &value).is_ge() => previous,
                    _ => value,
                });
            }
            WindowKind::AggCount(_) => {}
            _ => {
                return Err(ExecError::Other(
                    "non-aggregate window kind reached aggregate evaluation".into(),
                ))
            }
        }
    }
    Ok(Some(match kind {
        WindowKind::AggSum(_) => {
            if count == 0 {
                Value::Null
            } else {
                Value::Float(sum)
            }
        }
        WindowKind::AggCount(_) => Value::Int(count),
        WindowKind::AggAvg(_) => {
            if count == 0 {
                Value::Null
            } else {
                Value::Float(sum / count as f64)
            }
        }
        WindowKind::AggMin(_) => min.unwrap_or(Value::Null),
        WindowKind::AggMax(_) => max.unwrap_or(Value::Null),
        _ => {
            return Err(ExecError::Other(
                "non-aggregate window kind reached aggregate result construction".into(),
            ))
        }
    }))
}

fn builtin_ntile(index: u64, rows: u64, buckets: i64) -> ExecResult<Value> {
    let buckets = u64::try_from(buckets.max(1))
        .map_err(|_| ExecError::Other("NTILE bucket count is out of range".into()))?;
    let base = rows / buckets;
    let extra = rows % buckets;
    let larger_rows = if extra == 0 {
        0
    } else {
        base.checked_add(1)
            .and_then(|value| value.checked_mul(extra))
            .ok_or_else(|| ExecError::Other("NTILE partition size overflow".into()))?
    };
    let bucket = if index < larger_rows {
        index
            .checked_div(
                base.checked_add(1)
                    .ok_or_else(|| ExecError::Other("NTILE bucket width overflow".into()))?,
            )
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| ExecError::Other("NTILE bucket number overflow".into()))?
    } else if base == 0 {
        extra.max(1)
    } else {
        extra
            .checked_add(
                (index - larger_rows)
                    .checked_div(base)
                    .ok_or_else(|| ExecError::Other("invalid NTILE bucket width".into()))?,
            )
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| ExecError::Other("NTILE bucket number overflow".into()))?
    };
    Ok(Value::Int(i64::try_from(bucket).map_err(|_| {
        ExecError::Other("NTILE bucket number exceeds SQL integer range".into())
    })?))
}

fn emit_builtin_window_partition(
    partition: &mut crate::spill::IndexedSpill,
    spec: &WindowSpec,
    functions: &[(String, WindowKind)],
    params: &[SQLParam],
    schema: &RowSchema,
    output: &mut crate::spill::SpillBuffer,
) -> ExecResult<()> {
    let aggregate_values = functions
        .iter()
        .map(|(_, kind)| builtin_window_partition_value(kind, partition, params))
        .collect::<ExecResult<Vec<_>>>()?;
    let mut previous_order_key = None;
    let mut rank = 0_i64;
    let mut dense_rank = 0_i64;
    let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
    for index in 0..partition.len() {
        let mut row = partition.get(index)?;
        let order_key = builtin_window_order_key(&row, spec, params)?;
        if previous_order_key.as_ref() != Some(&order_key) {
            rank = i64::try_from(
                index
                    .checked_add(1)
                    .ok_or_else(|| ExecError::Other("window rank overflow".into()))?,
            )
            .map_err(|_| ExecError::Other("window rank exceeds SQL integer range".into()))?;
            dense_rank = dense_rank
                .checked_add(1)
                .ok_or_else(|| ExecError::Other("window dense rank overflow".into()))?;
        }
        for ((alias, kind), aggregate_value) in functions.iter().zip(&aggregate_values) {
            let value = match kind {
                WindowKind::RowNumber => {
                    Value::Int(
                        i64::try_from(index.checked_add(1).ok_or_else(|| {
                            ExecError::Other("window row number overflow".into())
                        })?)
                        .map_err(|_| {
                            ExecError::Other("window row number exceeds SQL integer range".into())
                        })?,
                    )
                }
                WindowKind::Rank => Value::Int(rank),
                WindowKind::DenseRank => Value::Int(dense_rank),
                WindowKind::Lag(expression, offset) | WindowKind::Lead(expression, offset) => {
                    let direction = if matches!(kind, WindowKind::Lag(..)) {
                        -1_i128
                    } else {
                        1_i128
                    };
                    let target = i128::from(index) + direction * i128::from(*offset);
                    if target < 0 || target >= i128::from(partition.len()) {
                        Value::Null
                    } else {
                        let target_row = partition.get(u64::try_from(target).map_err(|_| {
                            ExecError::Other("window offset target is out of range".into())
                        })?)?;
                        eval_scalar(
                            expression,
                            &ScalarEvalContext::new(Some(&target_row), params),
                        )?
                    }
                }
                WindowKind::Ntile(buckets) => builtin_ntile(index, partition.len(), *buckets)?,
                WindowKind::AggSum(_)
                | WindowKind::AggCount(_)
                | WindowKind::AggAvg(_)
                | WindowKind::AggMin(_)
                | WindowKind::AggMax(_) => aggregate_value.clone().ok_or_else(|| {
                    ExecError::Other("aggregate window value was not precomputed".into())
                })?,
            };
            row.insert(alias.clone(), value);
        }
        previous_order_key = Some(order_key);
        pending.push(row);
        if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
            output.push(Batch::new(schema.clone(), std::mem::take(&mut pending)))?;
            pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        }
    }
    if !pending.is_empty() {
        output.push(Batch::new(schema.clone(), pending))?;
    }
    Ok(())
}

impl PhysicalOperator for Window<'_> {
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
        let mut keys = self
            .spec
            .partition_by
            .iter()
            .cloned()
            .map(|expr| SortKey {
                expr,
                descending: false,
                nulls_first: None,
            })
            .collect::<Vec<_>>();
        keys.extend(self.spec.order_by.iter().cloned());
        let evaluator = DefaultExpressionEvaluator::shared(self.params.clone());
        let mut sorted =
            crate::external_sort::ExternalSort::new(scan, keys, evaluator, None, phase_budget);
        sorted.open()?;

        let mut current_partition_key: Option<Vec<Value>> = None;
        let mut partition = crate::spill::IndexedSpill::new()?;
        let mut output = crate::spill::SpillBuffer::new(phase_budget);
        let execution = (|| -> ExecResult<()> {
            while let Some(batch) = sorted.next()? {
                for row in batch.rows {
                    let context = ScalarEvalContext::new(Some(&row), &self.params);
                    let key = self
                        .spec
                        .partition_by
                        .iter()
                        .map(|expression| eval_scalar(expression, &context))
                        .collect::<Result<Vec<_>, _>>()?;
                    if current_partition_key
                        .as_ref()
                        .is_some_and(|current| current != &key)
                    {
                        emit_builtin_window_partition(
                            &mut partition,
                            &self.spec,
                            &self.functions,
                            &self.params,
                            &self.schema,
                            &mut output,
                        )?;
                        partition = crate::spill::IndexedSpill::new()?;
                    }
                    current_partition_key = Some(key);
                    partition.push(&row)?;
                }
            }
            if !partition.is_empty() {
                emit_builtin_window_partition(
                    &mut partition,
                    &self.spec,
                    &self.functions,
                    &self.params,
                    &self.schema,
                    &mut output,
                )?;
            }
            Ok(())
        })();
        let close = sorted.close();
        crate::physical::with_cleanup(execution, close, "close window sort after failure")?;
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
