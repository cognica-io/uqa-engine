//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! External sort wrapper and SQL key comparison.

use super::{
    Batch, DefaultExpressionEvaluator, ExecResult, PhysicalOperator, SQLParam, ScalarExpr,
    SharedExpressionEvaluator, Value,
};

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
        mut keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
        work_mem_bytes: usize,
    ) -> Self {
        for key in &mut keys {
            let expression = std::mem::replace(&mut key.expr, ScalarExpr::Literal(Value::Null));
            key.expr = evaluator.bind_type_introspection(expression, child.row_schema());
        }
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
        mut keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
        keep: usize,
    ) -> Self {
        for key in &mut keys {
            let expression = std::mem::replace(&mut key.expr, ScalarExpr::Literal(Value::Null));
            key.expr = evaluator.bind_type_introspection(expression, child.row_schema());
        }
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
    compare_sort_key_values_by(keys, |index| (&av[index], &bv[index]))
}

pub(crate) fn compare_sort_key_values_by<'a>(
    keys: &[SortKey],
    mut values: impl FnMut(usize) -> (&'a Value, &'a Value),
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (i, k) in keys.iter().enumerate() {
        let (a, b) = values(i);
        let a_null = matches!(a, Value::Null);
        let b_null = matches!(b, Value::Null);
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
        let ord = compare_values(a, b);
        let ord = if k.descending { ord.reverse() } else { ord };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

pub(super) fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
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
    fn row_schema(&self) -> &super::RowSchema {
        self.inner.row_schema()
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.inner.output_ordering()
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
