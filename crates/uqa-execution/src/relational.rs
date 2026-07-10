//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational Volcano operators: filter, project, sort, limit,
//! hash aggregate, and window. Each operator owns its child as a boxed
//! [`PhysicalOperator`] so trees can be assembled at runtime by the
//! planner without monomorphisation per shape.

use std::collections::BTreeMap;

use uqa_core::{DecimalValue, Value};
use uqa_sql::expr::{eval, truthy, EvalContext};
use uqa_sql::ResultRow;
use uqa_sql::{ast::Expr, SQLError, SQLParam};

use crate::batch::{Batch, RowSchema};
use crate::physical::{ExecError, ExecResult, PhysicalOperator};

// -------------------------------------------------------------------------
// Filter
// -------------------------------------------------------------------------

/// Pipelined `WHERE` operator. Drops rows whose predicate evaluates
/// to `false` or `NULL`; truthy rows pass through unchanged.
pub struct Filter {
    child: Box<dyn PhysicalOperator>,
    predicate: Expr,
    params: Vec<SQLParam>,
    schema: RowSchema,
}

impl Filter {
    pub fn new(child: Box<dyn PhysicalOperator>, predicate: Expr, params: Vec<SQLParam>) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            predicate,
            params,
            schema,
        }
    }
}

impl PhysicalOperator for Filter {
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
                let ctx = EvalContext::new(Some(&row), &self.params);
                if eval(&self.predicate, &ctx).is_ok_and(|v| truthy(&v)) {
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
pub struct Project {
    child: Box<dyn PhysicalOperator>,
    projections: Vec<(String, Expr)>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    /// When `true`, every input column also flows through to the
    /// output (after any alias rewrite). Useful when projections only
    /// derive new columns.
    pass_through: bool,
}

impl Project {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, Expr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        let schema = RowSchema::new(projections.iter().map(|(name, _)| name.clone()).collect());
        Self {
            child,
            projections,
            params,
            schema,
            pass_through: false,
        }
    }

    /// Variant that keeps every input column in the output and appends
    /// the projections at the end. Used by aggregate / window paths.
    pub fn appending(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, Expr)>,
        params: Vec<SQLParam>,
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
            params,
            schema,
            pass_through: true,
        }
    }
}

impl PhysicalOperator for Project {
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
            let ctx = EvalContext::new(Some(&row), &self.params);
            for (name, expr) in &self.projections {
                let v = eval(expr, &ctx)?;
                new_row.insert(name.clone(), v);
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
    pub expr: Expr,
    pub descending: bool,
    /// `Some(true)` forces NULLS FIRST, `Some(false)` forces NULLS
    /// LAST. `None` falls back to the SQL-standard default - NULLS
    /// LAST for ASC and NULLS FIRST for DESC.
    pub nulls_first: Option<bool>,
}

/// Blocking sort. Pulls every row from the child during `open`, then
/// emits the sorted output in [`crate::batch::DEFAULT_BATCH_SIZE`]-sized batches.
pub struct Sort {
    child: Box<dyn PhysicalOperator>,
    keys: Vec<SortKey>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    /// When set, only the first `keep` rows of the sorted output are
    /// retained (top-K selection instead of a full sort). Callers pass
    /// `OFFSET + LIMIT` so a downstream [`Limit`] can still skip.
    keep: Option<usize>,
    materialised: Option<std::vec::IntoIter<Batch>>,
}

impl Sort {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        keys: Vec<SortKey>,
        params: Vec<SQLParam>,
    ) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            keys,
            params,
            schema,
            keep: None,
            materialised: None,
        }
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
        let mut sort = Self::new(child, keys, params);
        sort.keep = Some(keep);
        sort
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
            let null_cmp = match (a_null, b_null) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (false, false) => unreachable!(),
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
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Decimal(x), Value::Decimal(y)) => x.cmp(y),
        (Value::Decimal(x), Value::Int(y)) => x.cmp(&DecimalValue::from_i64(*y)),
        (Value::Int(x), Value::Decimal(y)) => DecimalValue::from_i64(*x).cmp(y),
        (Value::Decimal(x), Value::Float(y)) => DecimalValue::from_f64_lossy(*y)
            .map(|yd| x.cmp(&yd))
            .unwrap_or(Equal),
        (Value::Float(x), Value::Decimal(y)) => DecimalValue::from_f64_lossy(*x)
            .map(|xd| xd.cmp(y))
            .unwrap_or(Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Str(y)) => {
            x.parse_same_kind(y).map_or(Equal, |parsed| x.cmp(&parsed))
        }
        (Value::Str(x), Value::Temporal(y)) => {
            y.parse_same_kind(x).map_or(Equal, |parsed| parsed.cmp(y))
        }
        _ => Equal,
    }
}

impl PhysicalOperator for Sort {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()?;
        let mut rows: Vec<ResultRow> = Vec::new();
        while let Some(batch) = self.child.next()? {
            rows.extend(batch.rows);
        }
        // Materialise sort keys per row, then sort by them.
        let mut decorated: Vec<(Vec<Value>, ResultRow)> = Vec::with_capacity(rows.len());
        for row in rows {
            let ctx = EvalContext::new(Some(&row), &self.params);
            let mut key_vals = Vec::with_capacity(self.keys.len());
            for k in &self.keys {
                key_vals.push(eval(&k.expr, &ctx)?);
            }
            decorated.push((key_vals, row));
        }
        if let Some(keep) = self.keep.filter(|keep| *keep < decorated.len()) {
            if keep == 0 {
                decorated.clear();
            } else {
                decorated.select_nth_unstable_by(keep - 1, |(av, _), (bv, _)| {
                    compare_sort_key_values(&self.keys, av, bv)
                });
                decorated.truncate(keep);
            }
        }
        decorated.sort_by(|(av, _), (bv, _)| compare_sort_key_values(&self.keys, av, bv));
        let sorted: Vec<ResultRow> = decorated.into_iter().map(|(_, r)| r).collect();
        let batches = Batch::chunked(self.schema.clone(), sorted);
        self.materialised = Some(batches.into_iter());
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(it) = self.materialised.as_mut() else {
            return Ok(None);
        };
        Ok(it.next())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.materialised = None;
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Limit / Offset
// -------------------------------------------------------------------------

pub struct Limit {
    child: Box<dyn PhysicalOperator>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    emitted: u64,
    schema: RowSchema,
}

impl Limit {
    pub fn new(child: Box<dyn PhysicalOperator>, offset: u64, limit: Option<u64>) -> Self {
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

impl PhysicalOperator for Limit {
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
    pub arg: Option<Expr>,
    /// Output column alias.
    pub alias: String,
    /// `COUNT(DISTINCT x)` / `SUM(DISTINCT x)` / etc.
    pub distinct: bool,
}

/// Blocking group-by + aggregate. Pulls every row from the child
/// during `open`, hashes each row by its group key, and folds the
/// aggregates over each group's row set. Groups are emitted in the
/// order they were first observed.
pub struct HashAggregate {
    child: Box<dyn PhysicalOperator>,
    group_keys: Vec<(String, Expr)>,
    aggregates: Vec<AggregateSpec>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    materialised: Option<std::vec::IntoIter<Batch>>,
}

impl HashAggregate {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        group_keys: Vec<(String, Expr)>,
        aggregates: Vec<AggregateSpec>,
        params: Vec<SQLParam>,
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
            materialised: None,
        }
    }
}

#[derive(Default)]
struct GroupState {
    /// Folded aggregate state, one slot per `aggregates` entry.
    folds: Vec<AggFold>,
    /// Group key values, captured on first row.
    key_values: Vec<Value>,
}

#[derive(Default, Clone)]
struct AggFold {
    count: u64,
    sum: Option<f64>,
    min: Option<Value>,
    max: Option<Value>,
    distinct: std::collections::BTreeSet<String>,
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

fn distinct_key(v: &Value) -> String {
    // Cheap canonical encoding for `DISTINCT` bookkeeping.
    match v {
        Value::Null => "\x00".into(),
        Value::Int(i) => format!("i:{i}"),
        Value::Float(f) => format!("f:{f:.17}"),
        Value::Str(s) => format!("s:{s}"),
        Value::Bool(b) => format!("b:{b}"),
        Value::Temporal(t) => format!("t:{}", t.to_sql_string()),
        other => format!("o:{other:?}"),
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
            state.count += 1;
        }
        _ => {
            let arg = spec.arg.as_ref().ok_or_else(|| {
                ExecError::Other(format!(
                    "aggregate {:?} requires an argument expression",
                    spec.kind
                ))
            })?;
            let ctx = EvalContext::new(Some(row), params);
            let v = eval(arg, &ctx)?;
            if matches!(v, Value::Null) {
                return Ok(());
            }
            if spec.distinct {
                let key = distinct_key(&v);
                if !state.distinct.insert(key) {
                    return Ok(());
                }
            }
            match spec.kind {
                AggregateKind::Count => state.count += 1,
                AggregateKind::Sum | AggregateKind::Avg => {
                    let f = value_to_f64(&v).ok_or_else(|| {
                        ExecError::Other(format!("non-numeric input to SUM/AVG: {v:?}"))
                    })?;
                    state.sum = Some(state.sum.unwrap_or(0.0) + f);
                    state.count += 1;
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
                AggregateKind::CountStar => unreachable!(),
            }
        }
    }
    Ok(())
}

fn finalise_fold(state: &AggFold, spec: &AggregateSpec) -> Value {
    match spec.kind {
        AggregateKind::Count | AggregateKind::CountStar => Value::Int(state.count as i64),
        AggregateKind::Sum => state.sum.map(Value::Float).unwrap_or(Value::Null),
        AggregateKind::Avg => match (state.sum, state.count) {
            (Some(s), c) if c > 0 => Value::Float(s / c as f64),
            _ => Value::Null,
        },
        AggregateKind::Min => state.min.clone().unwrap_or(Value::Null),
        AggregateKind::Max => state.max.clone().unwrap_or(Value::Null),
    }
}

impl PhysicalOperator for HashAggregate {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()?;
        let mut groups: BTreeMap<String, GroupState> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        while let Some(batch) = self.child.next()? {
            for row in batch.rows {
                let ctx = EvalContext::new(Some(&row), &self.params);
                let mut key_vals: Vec<Value> = Vec::with_capacity(self.group_keys.len());
                let mut key_repr = String::new();
                for (i, (_, expr)) in self.group_keys.iter().enumerate() {
                    let v = eval(expr, &ctx)?;
                    if i > 0 {
                        key_repr.push('\x1f');
                    }
                    key_repr.push_str(&distinct_key(&v));
                    key_vals.push(v);
                }
                let st = groups.entry(key_repr.clone()).or_insert_with(|| {
                    order.push(key_repr.clone());
                    GroupState {
                        folds: vec![AggFold::default(); self.aggregates.len()],
                        key_values: key_vals.clone(),
                    }
                });
                for (i, spec) in self.aggregates.iter().enumerate() {
                    fold_into(&mut st.folds[i], spec, &row, &self.params)?;
                }
            }
        }
        if groups.is_empty() && self.group_keys.is_empty() {
            // SQL: scalar aggregate over empty input still yields one
            // row with COUNT=0 and other aggregates NULL.
            let mut out_row = ResultRow::new();
            for (i, spec) in self.aggregates.iter().enumerate() {
                let _ = i;
                out_row.insert(spec.alias.clone(), finalise_fold(&AggFold::default(), spec));
            }
            let batches = Batch::chunked(self.schema.clone(), vec![out_row]);
            self.materialised = Some(batches.into_iter());
            return Ok(());
        }
        let mut out_rows: Vec<ResultRow> = Vec::with_capacity(order.len());
        for key in order {
            let st = groups.remove(&key).expect("group present");
            let mut out = ResultRow::new();
            for (i, (alias, _)) in self.group_keys.iter().enumerate() {
                out.insert(alias.clone(), st.key_values[i].clone());
            }
            for (i, spec) in self.aggregates.iter().enumerate() {
                out.insert(spec.alias.clone(), finalise_fold(&st.folds[i], spec));
            }
            out_rows.push(out);
        }
        let batches = Batch::chunked(self.schema.clone(), out_rows);
        self.materialised = Some(batches.into_iter());
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(it) = self.materialised.as_mut() else {
            return Ok(None);
        };
        Ok(it.next())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.materialised = None;
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
    Lag(Expr, i64),
    Lead(Expr, i64),
    Ntile(i64),
    AggSum(Expr),
    AggCount(Option<Expr>),
    AggAvg(Expr),
    AggMin(Expr),
    AggMax(Expr),
}

#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<SortKey>,
}

/// Window operator. Currently emits the entire input as a single batch
/// after appending one column per `(alias, kind)`. Deterministic
/// for tests and quickstart-class workloads; large inputs flow into
/// the [`crate::spill::SpillBuffer`] when wired up by the planner.
pub struct Window {
    child: Box<dyn PhysicalOperator>,
    spec: WindowSpec,
    functions: Vec<(String, WindowKind)>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    out: Option<Batch>,
    served: bool,
}

impl Window {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        spec: WindowSpec,
        functions: Vec<(String, WindowKind)>,
        params: Vec<SQLParam>,
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
            out: None,
            served: false,
        }
    }
}

impl PhysicalOperator for Window {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()?;
        self.served = false;
        let mut rows: Vec<ResultRow> = Vec::new();
        while let Some(batch) = self.child.next()? {
            rows.extend(batch.rows);
        }
        // Group rows by the PARTITION BY key, preserving insertion order
        // within a partition. Pure aggregates collapse to a single value
        // per partition; ranking functions need the ORDER BY to be
        // applied first.
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut bucket_order: Vec<String> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let ctx = EvalContext::new(Some(row), &self.params);
            let mut key = String::new();
            for (j, p) in self.spec.partition_by.iter().enumerate() {
                if j > 0 {
                    key.push('\x1f');
                }
                let v = eval(p, &ctx)?;
                key.push_str(&distinct_key(&v));
            }
            buckets.entry(key.clone()).or_insert_with(|| {
                bucket_order.push(key.clone());
                Vec::new()
            });
            buckets.get_mut(&key).unwrap().push(i);
        }
        // Stable order-by within each partition.
        for indices in buckets.values_mut() {
            let keys: Result<Vec<Vec<Value>>, SQLError> = indices
                .iter()
                .map(|&i| {
                    let ctx = EvalContext::new(Some(&rows[i]), &self.params);
                    let mut k = Vec::with_capacity(self.spec.order_by.len());
                    for s in &self.spec.order_by {
                        k.push(eval(&s.expr, &ctx)?);
                    }
                    Ok(k)
                })
                .collect();
            let keys = keys?;
            let mut decorated: Vec<(Vec<Value>, usize)> =
                keys.into_iter().zip(indices.iter().copied()).collect();
            decorated.sort_by(|a, b| {
                for (i, k) in self.spec.order_by.iter().enumerate() {
                    let ord = compare_values(&a.0[i], &b.0[i]);
                    let ord = if k.descending { ord.reverse() } else { ord };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });
            *indices = decorated.into_iter().map(|(_, i)| i).collect();
        }
        // Compute window function outputs into a parallel column map
        // keyed by the row index.
        let mut overlay: Vec<ResultRow> = vec![ResultRow::new(); rows.len()];
        for key in &bucket_order {
            let indices = buckets.get(key).expect("bucket");
            for (alias, kind) in &self.functions {
                match kind {
                    WindowKind::RowNumber => {
                        for (rank, &row_idx) in indices.iter().enumerate() {
                            overlay[row_idx].insert(alias.clone(), Value::Int(rank as i64 + 1));
                        }
                    }
                    WindowKind::Rank => {
                        let mut last_keys: Option<Vec<Value>> = None;
                        let mut tie_block_start = 1i64;
                        for (i, &row_idx) in indices.iter().enumerate() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let mut k = Vec::with_capacity(self.spec.order_by.len());
                            for s in &self.spec.order_by {
                                k.push(eval(&s.expr, &ctx)?);
                            }
                            if let Some(prev) = last_keys.as_ref() {
                                if prev != &k {
                                    tie_block_start = i as i64 + 1;
                                }
                            }
                            overlay[row_idx].insert(alias.clone(), Value::Int(tie_block_start));
                            last_keys = Some(k);
                        }
                    }
                    WindowKind::DenseRank => {
                        let mut last_keys: Option<Vec<Value>> = None;
                        let mut current = 0i64;
                        for &row_idx in indices.iter() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let mut k = Vec::with_capacity(self.spec.order_by.len());
                            for s in &self.spec.order_by {
                                k.push(eval(&s.expr, &ctx)?);
                            }
                            match last_keys.as_ref() {
                                Some(prev) if prev == &k => {}
                                _ => current += 1,
                            }
                            overlay[row_idx].insert(alias.clone(), Value::Int(current));
                            last_keys = Some(k);
                        }
                    }
                    WindowKind::Lag(arg, off) => {
                        for (i, &row_idx) in indices.iter().enumerate() {
                            let pos = i as i64 - *off;
                            let v = if pos >= 0 && (pos as usize) < indices.len() {
                                let src_row = &rows[indices[pos as usize]];
                                let ctx = EvalContext::new(Some(src_row), &self.params);
                                eval(arg, &ctx)?
                            } else {
                                Value::Null
                            };
                            overlay[row_idx].insert(alias.clone(), v);
                        }
                    }
                    WindowKind::Lead(arg, off) => {
                        for (i, &row_idx) in indices.iter().enumerate() {
                            let pos = i as i64 + *off;
                            let v = if pos >= 0 && (pos as usize) < indices.len() {
                                let src_row = &rows[indices[pos as usize]];
                                let ctx = EvalContext::new(Some(src_row), &self.params);
                                eval(arg, &ctx)?
                            } else {
                                Value::Null
                            };
                            overlay[row_idx].insert(alias.clone(), v);
                        }
                    }
                    WindowKind::Ntile(n) => {
                        let total = indices.len() as i64;
                        let buckets_count = (*n).max(1);
                        let base = total / buckets_count;
                        let rem = total % buckets_count;
                        let mut bucket = 1i64;
                        let mut emitted = 0i64;
                        for &row_idx in indices.iter() {
                            let limit = base + i64::from(bucket <= rem);
                            if emitted >= limit {
                                bucket += 1;
                                emitted = 0;
                            }
                            overlay[row_idx].insert(alias.clone(), Value::Int(bucket));
                            emitted += 1;
                        }
                    }
                    WindowKind::AggSum(arg) => {
                        let mut sum = 0f64;
                        let mut any = false;
                        for &row_idx in indices.iter() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let v = eval(arg, &ctx)?;
                            if let Some(n) = value_to_f64(&v) {
                                sum += n;
                                any = true;
                            }
                        }
                        let out = if any { Value::Float(sum) } else { Value::Null };
                        for &row_idx in indices.iter() {
                            overlay[row_idx].insert(alias.clone(), out.clone());
                        }
                    }
                    WindowKind::AggCount(arg) => {
                        let mut c: i64 = 0;
                        for &row_idx in indices.iter() {
                            match arg {
                                None => c += 1,
                                Some(e) => {
                                    let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                                    if !matches!(eval(e, &ctx)?, Value::Null) {
                                        c += 1;
                                    }
                                }
                            }
                        }
                        for &row_idx in indices.iter() {
                            overlay[row_idx].insert(alias.clone(), Value::Int(c));
                        }
                    }
                    WindowKind::AggAvg(arg) => {
                        let mut sum = 0f64;
                        let mut count: i64 = 0;
                        for &row_idx in indices.iter() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let v = eval(arg, &ctx)?;
                            if let Some(n) = value_to_f64(&v) {
                                sum += n;
                                count += 1;
                            }
                        }
                        let out = if count > 0 {
                            Value::Float(sum / count as f64)
                        } else {
                            Value::Null
                        };
                        for &row_idx in indices.iter() {
                            overlay[row_idx].insert(alias.clone(), out.clone());
                        }
                    }
                    WindowKind::AggMin(arg) => {
                        let mut acc: Option<Value> = None;
                        for &row_idx in indices.iter() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let v = eval(arg, &ctx)?;
                            if matches!(v, Value::Null) {
                                continue;
                            }
                            acc = Some(match acc.take() {
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
                        let out = acc.unwrap_or(Value::Null);
                        for &row_idx in indices.iter() {
                            overlay[row_idx].insert(alias.clone(), out.clone());
                        }
                    }
                    WindowKind::AggMax(arg) => {
                        let mut acc: Option<Value> = None;
                        for &row_idx in indices.iter() {
                            let ctx = EvalContext::new(Some(&rows[row_idx]), &self.params);
                            let v = eval(arg, &ctx)?;
                            if matches!(v, Value::Null) {
                                continue;
                            }
                            acc = Some(match acc.take() {
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
                        let out = acc.unwrap_or(Value::Null);
                        for &row_idx in indices.iter() {
                            overlay[row_idx].insert(alias.clone(), out.clone());
                        }
                    }
                }
            }
        }
        // Merge overlay back onto rows.
        let merged: Vec<ResultRow> = rows
            .into_iter()
            .zip(overlay)
            .map(|(mut row, ov)| {
                for (k, v) in ov {
                    row.insert(k, v);
                }
                row
            })
            .collect();
        self.out = Some(Batch::new(self.schema.clone(), merged));
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.served {
            return Ok(None);
        }
        self.served = true;
        Ok(self.out.take())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.out = None;
        self.served = false;
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;
    use uqa_core::Value;
    use uqa_sql::ast::{BinaryOp, Expr};

    fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn boxed_scan(schema: Vec<String>, rows: Vec<ResultRow>) -> Box<dyn PhysicalOperator> {
        Box::new(TableScan::from_rows(schema, rows))
    }

    fn col(name: &str) -> Expr {
        Expr::Column(name.into())
    }

    fn bin(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Binary {
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
        let predicate = bin(BinaryOp::Greater, col("x"), Expr::Literal(Value::Int(1)));
        let mut filt = Filter::new(scan, predicate, vec![]);
        let (_cols, rows) = run_to_rows(&mut filt).unwrap();
        assert_eq!(rows.len(), 2);
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
}
