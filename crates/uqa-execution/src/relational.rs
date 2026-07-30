//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational Volcano operators: filter, project, sort, limit,
//! hash aggregate, and window. Each operator owns its child as a boxed
//! [`PhysicalOperator`] so trees can be assembled at runtime by the
//! planner without monomorphisation per shape.

use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::SetOpKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;
use uqa_sql::SQLParam;

use crate::batch::{Batch, RowSchema};
use crate::physical::{ExecError, ExecResult, PhysicalOperator};
use crate::scalar::{eval_scalar, ScalarEvalContext, ScalarExpr};

/// Runtime scalar semantics used by relational operators.
///
/// The execution crate provides the default SQL evaluator, while an engine can
/// supply the same physical expressions with its function registry and
/// subquery arena attached. Keeping this seam at the operator boundary avoids
/// pre-evaluating predicates, projections, or sort keys outside the Volcano
/// tree.
pub trait ExpressionEvaluator: Send + Sync {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value>;

    fn project_star(&self, row: &ResultRow) -> ExecResult<ResultRow> {
        Ok(row.clone())
    }
}

pub type SharedExpressionEvaluator<'a> = Arc<dyn ExpressionEvaluator + 'a>;

pub trait RowPredicate: Send + Sync {
    fn keep(&self, row: &ResultRow) -> ExecResult<bool>;
}

pub type SharedRowPredicate<'a> = Arc<dyn RowPredicate + 'a>;

struct DefaultExpressionEvaluator {
    params: Vec<SQLParam>,
}

impl DefaultExpressionEvaluator {
    fn shared(params: Vec<SQLParam>) -> SharedExpressionEvaluator<'static> {
        Arc::new(Self { params })
    }
}

impl ExpressionEvaluator for DefaultExpressionEvaluator {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
        let context = ScalarEvalContext::new(Some(row), &self.params);
        Ok(eval_scalar(expression, &context)?)
    }
}

// -------------------------------------------------------------------------
// Filter
// -------------------------------------------------------------------------

/// Pipelined `WHERE` operator. Drops rows whose predicate evaluates
/// to `false` or `NULL`; truthy rows pass through unchanged.
pub struct Filter<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    condition: FilterCondition<'a>,
    schema: RowSchema,
}

enum FilterCondition<'a> {
    Expression {
        predicate: ScalarExpr,
        evaluator: SharedExpressionEvaluator<'a>,
    },
    Row(SharedRowPredicate<'a>),
}

impl Filter<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        predicate: ScalarExpr,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(child, predicate, DefaultExpressionEvaluator::shared(params))
    }
}

impl<'a> Filter<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: ScalarExpr,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            condition: FilterCondition::Expression {
                predicate,
                evaluator,
            },
            schema,
        }
    }

    pub fn with_row_predicate(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: SharedRowPredicate<'a>,
    ) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            condition: FilterCondition::Row(predicate),
            schema,
        }
    }
}

impl PhysicalOperator for Filter<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            let mut kept = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                let keep = match &self.condition {
                    FilterCondition::Expression {
                        predicate,
                        evaluator,
                    } => truthy(&evaluator.evaluate(predicate, &row)?),
                    FilterCondition::Row(predicate) => predicate.keep(&row)?,
                };
                if keep {
                    kept.push(row);
                }
            }
            if !kept.is_empty() {
                return Ok(Some(Batch::new(self.schema.clone(), kept)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Project
// -------------------------------------------------------------------------

/// Per-row scalar projection. Each `(alias, expr)` pair is evaluated
/// against the input row and written under `alias` in the output. The
/// child schema is replaced with the output aliases.
pub struct Project<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    projections: Vec<(String, ScalarExpr)>,
    evaluator: SharedExpressionEvaluator<'a>,
    schema: RowSchema,
    /// When `true`, every input column also flows through to the
    /// output (after any alias rewrite). Useful when projections only
    /// derive new columns.
    pass_through: bool,
}

impl Project<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }

    /// Variant that keeps every input column in the output and appends
    /// the projections at the end. Used by aggregate / window paths.
    pub fn appending(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::appending_with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }
}

impl<'a> Project<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let mut columns = Vec::new();
        for (name, expression) in &projections {
            if matches!(expression, ScalarExpr::Star) {
                for column in child.schema() {
                    if !columns.contains(column) {
                        columns.push(column.clone());
                    }
                }
            } else {
                columns.push(name.clone());
            }
        }
        let schema = RowSchema::new(columns);
        Self {
            child,
            projections,
            evaluator,
            schema,
            pass_through: false,
        }
    }

    pub fn appending_with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let mut cols = child.schema().to_vec();
        for (name, _) in &projections {
            if !cols.contains(name) {
                cols.push(name.clone());
            }
        }
        let schema = RowSchema::new(cols);
        Self {
            child,
            projections,
            evaluator,
            schema,
            pass_through: true,
        }
    }
}

impl PhysicalOperator for Project<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let mut new_row: ResultRow = if self.pass_through {
                row.clone()
            } else {
                ResultRow::new()
            };
            for (name, expr) in &self.projections {
                if matches!(expr, ScalarExpr::Star) {
                    for (column, value) in self.evaluator.project_star(&row)? {
                        new_row.insert(column, value);
                    }
                } else {
                    let value = self.evaluator.evaluate(expr, &row)?;
                    new_row.insert(name.clone(), value);
                }
            }
            out.push(new_row);
        }
        Ok(Some(Batch::new(self.schema.clone(), out)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Sort
// -------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SortKey {
    pub expr: ScalarExpr,
    pub descending: bool,
    /// `Some(true)` forces NULLS FIRST, `Some(false)` forces NULLS
    /// LAST. `None` falls back to the SQL-standard default - NULLS
    /// LAST for ASC and NULLS FIRST for DESC.
    pub nulls_first: Option<bool>,
}

/// Byte-bounded blocking sort backed by the external merge-sort implementation.
/// Compatibility constructors use a 64 MiB budget; engine callers should pass
/// the active session's `work_mem` through [`Self::with_evaluator_and_work_mem`].
const DEFAULT_SORT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

pub struct Sort<'a> {
    inner: crate::external_sort::ExternalSort<'a>,
}

impl Sort<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        keys: Vec<SortKey>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(child, keys, DefaultExpressionEvaluator::shared(params))
    }

    /// Top-K variant: retain only the first `keep` rows of the sorted
    /// order. Uses a partial selection, so the cost is `O(n + k log k)`
    /// instead of `O(n log n)`.
    pub fn with_keep(
        child: Box<dyn PhysicalOperator>,
        keys: Vec<SortKey>,
        params: Vec<SQLParam>,
        keep: usize,
    ) -> Self {
        Self::with_evaluator_and_keep(
            child,
            keys,
            DefaultExpressionEvaluator::shared(params),
            keep,
        )
    }
}

impl<'a> Sort<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self::with_evaluator_and_work_mem(child, keys, evaluator, DEFAULT_SORT_WORK_MEM_BYTES)
    }

    pub fn with_evaluator_and_work_mem(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
        work_mem_bytes: usize,
    ) -> Self {
        Self {
            inner: crate::external_sort::ExternalSort::new(
                child,
                keys,
                evaluator,
                None,
                work_mem_bytes,
            ),
        }
    }

    pub fn with_evaluator_and_keep(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
        keep: usize,
    ) -> Self {
        Self {
            inner: crate::external_sort::ExternalSort::new(
                child,
                keys,
                evaluator,
                Some(keep),
                DEFAULT_SORT_WORK_MEM_BYTES,
            ),
        }
    }
}

/// Compare two pre-computed sort-key vectors under `keys` semantics:
/// per-key direction plus `PostgreSQL` NULLS placement (default NULLS
/// LAST for ascending, NULLS FIRST for descending).
pub fn compare_sort_key_values(keys: &[SortKey], av: &[Value], bv: &[Value]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (i, k) in keys.iter().enumerate() {
        let a_null = matches!(av[i], Value::Null);
        let b_null = matches!(bv[i], Value::Null);
        let nulls_first = k.nulls_first.unwrap_or(k.descending);
        if a_null || b_null {
            let null_cmp = if a_null == b_null {
                Ordering::Equal
            } else if a_null {
                if nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            } else if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            };
            if null_cmp != Ordering::Equal {
                return null_cmp;
            }
            continue;
        }
        let ord = compare_values(&av[i], &bv[i]);
        let ord = if k.descending { ord.reverse() } else { ord };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map_or_else(|| a.cmp(b), |parsed| x.cmp(&parsed)),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map_or_else(|| a.cmp(b), |parsed| parsed.cmp(y)),
        _ => a.cmp(b),
    }
}

impl PhysicalOperator for Sort<'_> {
    fn schema(&self) -> &[String] {
        self.inner.schema()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.inner.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.inner.next()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.inner.close()
    }
}

// -------------------------------------------------------------------------
// Limit / Offset
// -------------------------------------------------------------------------

pub struct Limit<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    emitted: u64,
    schema: RowSchema,
}

impl<'a> Limit<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, offset: u64, limit: Option<u64>) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            offset,
            limit,
            skipped: 0,
            emitted: 0,
            schema,
        }
    }
}

impl PhysicalOperator for Limit<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.skipped = 0;
        self.emitted = 0;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if matches!(self.limit, Some(0)) {
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
                        return if buf.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(Batch::new(self.schema.clone(), buf)))
                        };
                    }
                }
                buf.push(row);
                self.emitted += 1;
            }
            if !buf.is_empty() {
                return Ok(Some(Batch::new(self.schema.clone(), buf)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Set operations
// -------------------------------------------------------------------------

/// Byte-bounded compatibility wrapper for SQL set operations.
///
/// All forms other than `UNION ALL` externally sort and merge their inputs;
/// `UNION ALL` streams both children. Construction is fallible because input
/// widths must agree.
pub struct SetOperation<'a> {
    inner: crate::set_operation::ExternalSetOperation<'a>,
}

impl<'a> SetOperation<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
    ) -> ExecResult<Self> {
        Self::new_with_work_mem(left, right, kind, all, 64 * 1024 * 1024)
    }

    pub fn new_with_work_mem(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        Ok(Self {
            inner: crate::set_operation::ExternalSetOperation::new(
                left,
                right,
                kind,
                all,
                work_mem_bytes,
            )?,
        })
    }
}

impl PhysicalOperator for SetOperation<'_> {
    fn schema(&self) -> &[String] {
        self.inner.schema()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.inner.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.inner.next()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.inner.close()
    }
}

// -------------------------------------------------------------------------
// Hash aggregate
// -------------------------------------------------------------------------

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

struct AggFold {
    count: u64,
    sum: Option<f64>,
    min: Option<Value>,
    max: Option<Value>,
    distinct: crate::distinct::SeenKeySet,
}

impl AggFold {
    fn new(work_mem_bytes: usize) -> Self {
        Self {
            count: 0,
            sum: None,
            min: None,
            max: None,
            distinct: crate::distinct::SeenKeySet::new(work_mem_bytes, None),
        }
    }
}

fn value_to_f64(v: &Value) -> Option<f64> {
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

fn finalise_fold(state: &AggFold, spec: &AggregateSpec) -> ExecResult<Value> {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;
    use uqa_core::Value;
    use uqa_sql::ast::BinaryOp;

    fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn boxed_scan(schema: Vec<String>, rows: Vec<ResultRow>) -> Box<dyn PhysicalOperator> {
        Box::new(TableScan::from_rows(schema, rows))
    }

    fn col(name: &str) -> ScalarExpr {
        ScalarExpr::Column(name.into())
    }

    fn bin(op: BinaryOp, lhs: ScalarExpr, rhs: ScalarExpr) -> ScalarExpr {
        ScalarExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    #[test]
    fn filter_keeps_truthy_rows() {
        let scan = boxed_scan(
            vec!["x".into()],
            vec![
                row([("x", Value::Int(1))]),
                row([("x", Value::Int(2))]),
                row([("x", Value::Int(3))]),
            ],
        );
        let predicate = bin(
            BinaryOp::Greater,
            col("x"),
            ScalarExpr::Literal(Value::Int(1)),
        );
        let mut filt = Filter::new(scan, predicate, vec![]);
        let (_cols, rows) = run_to_rows(&mut filt).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn filter_propagates_expression_errors() {
        let scan = boxed_scan(vec!["x".into()], vec![row([("x", Value::Int(1))])]);
        let zero = bin(BinaryOp::Subtract, col("x"), col("x"));
        let division = bin(BinaryOp::Divide, col("x"), zero);
        let predicate = bin(
            BinaryOp::Greater,
            division,
            ScalarExpr::Literal(Value::Int(0)),
        );
        let mut filter = Filter::new(scan, predicate, vec![]);
        let error = run_to_rows(&mut filter).unwrap_err();
        assert!(error.to_string().contains("division by zero"));
    }

    #[test]
    fn limit_with_offset() {
        let scan = boxed_scan(
            vec!["x".into()],
            (0..10)
                .map(|i| row([("x", Value::Int(i as i64))]))
                .collect(),
        );
        let mut lim = Limit::new(scan, 3, Some(4));
        let (_cols, rows) = run_to_rows(&mut lim).unwrap();
        assert_eq!(rows.len(), 4);
        let xs: Vec<i64> = rows
            .iter()
            .map(|r| match r["x"] {
                Value::Int(i) => i,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(xs, vec![3, 4, 5, 6]);
    }

    #[test]
    fn sort_descending() {
        let scan = boxed_scan(
            vec!["x".into()],
            vec![
                row([("x", Value::Int(2))]),
                row([("x", Value::Int(1))]),
                row([("x", Value::Int(3))]),
            ],
        );
        let mut sort = Sort::new(
            scan,
            vec![SortKey {
                expr: col("x"),
                descending: true,
                nulls_first: None,
            }],
            vec![],
        );
        let (_cols, rows) = run_to_rows(&mut sort).unwrap();
        let xs: Vec<i64> = rows
            .iter()
            .map(|r| match r["x"] {
                Value::Int(i) => i,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(xs, vec![3, 2, 1]);
    }

    #[test]
    fn physical_sort_comparison_preserves_numeric_total_order() {
        assert_eq!(
            compare_values(
                &Value::Int(9_007_199_254_740_993),
                &Value::Float(9_007_199_254_740_992.0),
            ),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_values(&Value::Float(f64::NAN), &Value::Float(f64::INFINITY)),
            std::cmp::Ordering::Greater
        );
        assert_ne!(
            compare_values(&Value::Bytes(vec![1]), &Value::Bytes(vec![2])),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn hash_aggregate_count_sum_per_group() {
        let scan = boxed_scan(
            vec!["g".into(), "v".into()],
            vec![
                row([("g", Value::Str("a".into())), ("v", Value::Int(1))]),
                row([("g", Value::Str("a".into())), ("v", Value::Int(2))]),
                row([("g", Value::Str("b".into())), ("v", Value::Int(5))]),
            ],
        );
        let agg = HashAggregate::new(
            scan,
            vec![("g".into(), col("g"))],
            vec![
                AggregateSpec {
                    kind: AggregateKind::Count,
                    arg: Some(col("v")),
                    alias: "n".into(),
                    distinct: false,
                },
                AggregateSpec {
                    kind: AggregateKind::Sum,
                    arg: Some(col("v")),
                    alias: "total".into(),
                    distinct: false,
                },
            ],
            vec![],
        );
        let mut agg = agg;
        let (_cols, rows) = run_to_rows(&mut agg).unwrap();
        assert_eq!(rows.len(), 2);
        let by_group: BTreeMap<String, &ResultRow> = rows
            .iter()
            .map(|r| match &r["g"] {
                Value::Str(s) => (s.clone(), r),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(by_group["a"]["n"], Value::Int(2));
        assert_eq!(by_group["a"]["total"], Value::Float(3.0));
        assert_eq!(by_group["b"]["n"], Value::Int(1));
        assert_eq!(by_group["b"]["total"], Value::Float(5.0));
    }

    #[test]
    fn aggregate_count_finalizer_rejects_bigint_overflow() {
        let mut fold = AggFold::new(1);
        fold.count = i64::MAX as u64 + 1;
        let spec = AggregateSpec {
            kind: AggregateKind::CountStar,
            arg: None,
            alias: "count".into(),
            distinct: false,
        };
        assert!(finalise_fold(&fold, &spec)
            .unwrap_err()
            .to_string()
            .contains("exceeds BIGINT"));
    }

    #[test]
    fn hash_aggregate_tiny_budget_spills_input_groups_and_distinct_state() {
        let rows = (0..512_i64)
            .map(|value| row([("g", Value::Int(value % 64)), ("v", Value::Int(value % 17))]))
            .collect();
        let scan = boxed_scan(vec!["g".into(), "v".into()], rows);
        let mut aggregate = HashAggregate::new_with_work_mem(
            scan,
            vec![("g".into(), col("g"))],
            vec![AggregateSpec {
                kind: AggregateKind::Count,
                arg: Some(col("v")),
                alias: "unique_values".into(),
                distinct: true,
            }],
            vec![],
            1,
        );
        let (_, rows) = run_to_rows(&mut aggregate).unwrap();
        assert!(aggregate.output_has_spilled());
        assert_eq!(rows.len(), 64);
        assert!(rows.iter().all(|row| {
            matches!(row.get("unique_values"), Some(Value::Int(count)) if *count > 0)
        }));
    }

    #[test]
    fn window_row_number_dense_rank() {
        let scan = boxed_scan(
            vec!["g".into(), "v".into()],
            vec![
                row([("g", Value::Str("a".into())), ("v", Value::Int(10))]),
                row([("g", Value::Str("a".into())), ("v", Value::Int(20))]),
                row([("g", Value::Str("a".into())), ("v", Value::Int(20))]),
                row([("g", Value::Str("b".into())), ("v", Value::Int(7))]),
            ],
        );
        let win = Window::new(
            scan,
            WindowSpec {
                partition_by: vec![col("g")],
                order_by: vec![SortKey {
                    expr: col("v"),
                    descending: false,
                    nulls_first: None,
                }],
            },
            vec![
                ("rn".into(), WindowKind::RowNumber),
                ("dr".into(), WindowKind::DenseRank),
            ],
            vec![],
        );
        let mut win = win;
        let (_cols, rows) = run_to_rows(&mut win).unwrap();
        assert_eq!(rows.len(), 4);
        // partition `a` ordered by v ascending: 10, 20, 20.
        let part_a: Vec<&ResultRow> = rows
            .iter()
            .filter(|r| matches!(&r["g"], Value::Str(s) if s == "a"))
            .collect();
        assert_eq!(part_a.len(), 3);
        let row_for_v = |v: i64| -> &ResultRow {
            *part_a
                .iter()
                .find(|r| matches!(r["v"], Value::Int(x) if x == v))
                .unwrap()
        };
        assert_eq!(row_for_v(10)["dr"], Value::Int(1));
        // Two ties on v=20 share a dense rank of 2.
        let twenties: Vec<&&ResultRow> = part_a
            .iter()
            .filter(|r| matches!(r["v"], Value::Int(20)))
            .collect();
        assert_eq!(twenties.len(), 2);
        for r in twenties {
            assert_eq!(r["dr"], Value::Int(2));
        }
    }

    #[test]
    fn window_tiny_budget_uses_disk_partition_for_random_access() {
        let rows = (0..512_i64)
            .map(|value| row([("g", Value::Int(1)), ("v", Value::Int(value))]))
            .collect();
        let scan = boxed_scan(vec!["g".into(), "v".into()], rows);
        let mut window = Window::new_with_work_mem(
            scan,
            WindowSpec {
                partition_by: vec![col("g")],
                order_by: vec![SortKey {
                    expr: col("v"),
                    descending: false,
                    nulls_first: None,
                }],
            },
            vec![
                ("rn".into(), WindowKind::RowNumber),
                ("next".into(), WindowKind::Lead(col("v"), 1)),
                ("total".into(), WindowKind::AggSum(col("v"))),
            ],
            vec![],
            1,
        );
        let (_, rows) = run_to_rows(&mut window).unwrap();
        assert!(window.output_has_spilled());
        assert_eq!(rows.len(), 512);
        assert_eq!(rows[0].get("rn"), Some(&Value::Int(1)));
        assert_eq!(rows[0].get("next"), Some(&Value::Int(1)));
        assert_eq!(rows[511].get("next"), Some(&Value::Null));
        assert_eq!(
            rows[511].get("total"),
            Some(&Value::Float((0..512_i64).sum::<i64>() as f64))
        );
    }
}
