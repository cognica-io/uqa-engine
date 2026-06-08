//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational joins over `ResultRow` row sets.
//!
//! Implements the row-join behavior from UQA `joins/inner`,
//! UQA `joins/outer`, UQA `joins/semi`, UQA `joins/cross`,
//! UQA `joins/sort_merge`, UQA `joins/index`. The UQA-RS implementation
//! operates on `ResultRow` (`BTreeMap<String, Value>`) directly so it
//! can be plugged into the engine's row-tuple SQL pipeline without an
//! extra adapter; the returned `Vec<ResultRow>` carries qualifier-
//! prefixed columns that the projection layer consumes verbatim.

use std::collections::HashMap;

use uqa_core::Value;
use uqa_sql::ResultRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
    Semi,
    Anti,
    Cross,
}

/// A canonical, hashable representation of a join-key value. Keeps the
/// hash join's lookup map fast and avoids re-hashing the underlying
/// `Value` for every probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JoinKey {
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    Bytes(Vec<u8>),
    Other(String),
    Composite(Vec<JoinKey>),
}

impl JoinKey {
    pub fn new(value: &Value) -> Self {
        match value {
            Value::Bool(value) => Self::Bool(*value),
            Value::Int(value) => Self::Int(*value),
            Value::Float(value) => Self::Float(normalized_float_bits(*value)),
            Value::Str(value) => Self::Str(value.clone()),
            Value::Bytes(value) => Self::Bytes(value.clone()),
            other => Self::Other(encode_fallback(other)),
        }
    }

    /// Composite key for multi-column equijoins.
    pub fn composite(values: &[&Value]) -> Self {
        Self::Composite(values.iter().map(|value| Self::new(value)).collect())
    }
}

fn normalized_float_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn encode_fallback(v: &Value) -> String {
    match v {
        Value::Null => "\x00".into(),
        Value::Temporal(value) => format!("t:{}", value.to_sql_string()),
        other => format!("o:{other:?}"),
    }
}

/// Merge `left` and `right` into a fresh row, preserving column order
/// by inserting `left` first, then non-conflicting `right` columns. SQL
/// projection later picks out the columns it cares about by name.
fn merge(left: &ResultRow, right: &ResultRow) -> ResultRow {
    if left.len() >= right.len() {
        let mut out = left.clone();
        for (k, v) in right {
            out.insert(k.clone(), v.clone());
        }
        out
    } else {
        let mut out = right.clone();
        for (k, v) in left {
            out.entry(k.clone()).or_insert_with(|| v.clone());
        }
        out
    }
}

/// Construct a row containing every column from `row` and each
/// `column` from the right side filled with `Value::Null`.
fn pad_with_nulls(row: &ResultRow, columns: &[String]) -> ResultRow {
    let mut out = row.clone();
    for c in columns {
        out.entry(c.clone()).or_insert(Value::Null);
    }
    out
}

fn collect_columns(rows: &[ResultRow]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for r in rows {
        for k in r.keys() {
            if !seen.iter().any(|c| c == k) {
                seen.push(k.clone());
            }
        }
    }
    seen
}

fn build_index<'a, F>(rows: &'a [ResultRow], key_of: F) -> HashMap<JoinKey, Vec<&'a ResultRow>>
where
    F: Fn(&'a ResultRow) -> Option<JoinKey>,
{
    let mut idx: HashMap<JoinKey, Vec<&'a ResultRow>> = HashMap::with_capacity(rows.len());
    for row in rows {
        if let Some(key) = key_of(row) {
            idx.entry(key).or_default().push(row);
        }
    }
    idx
}

// -------------------------------------------------------------------------
// Hash inner / outer
// -------------------------------------------------------------------------

/// Single-key equijoin hash inner join. The `(left, right)` keying
/// function lets callers do qualifier-prefixed lookups
/// (e.g. `t1.id == t2.user_id`) without an intermediate Vec.
pub fn hash_inner_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let (build_rows, probe_rows, build_is_left) = if left.len() <= right.len() {
        (left, right, true)
    } else {
        (right, left, false)
    };
    let build_key_fn: &dyn Fn(&ResultRow) -> Option<JoinKey> =
        if build_is_left { &left_key } else { &right_key };
    let probe_key_fn: &dyn Fn(&ResultRow) -> Option<JoinKey> =
        if build_is_left { &right_key } else { &left_key };
    let index = build_index(build_rows, build_key_fn);
    let mut out: Vec<ResultRow> = Vec::with_capacity(probe_rows.len());
    for probe in probe_rows {
        let Some(key) = probe_key_fn(probe) else {
            continue;
        };
        if let Some(matches) = index.get(&key) {
            for build_row in matches {
                let merged = if build_is_left {
                    merge(build_row, probe)
                } else {
                    merge(probe, build_row)
                };
                out.push(merged);
            }
        }
    }
    out
}

pub fn left_outer_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let right_columns = collect_columns(right);
    let index = build_index(right, &right_key);
    let mut out: Vec<ResultRow> = Vec::with_capacity(left.len());
    for l in left {
        let key = left_key(l);
        let matches = key.as_ref().and_then(|k| index.get(k));
        match matches {
            Some(rows) if !rows.is_empty() => {
                for r in rows {
                    out.push(merge(l, r));
                }
            }
            _ => out.push(pad_with_nulls(l, &right_columns)),
        }
    }
    out
}

pub fn right_outer_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let left_columns = collect_columns(left);
    let index = build_index(left, &left_key);
    let mut out: Vec<ResultRow> = Vec::with_capacity(right.len());
    for r in right {
        let key = right_key(r);
        let matches = key.as_ref().and_then(|k| index.get(k));
        match matches {
            Some(rows) if !rows.is_empty() => {
                for l in rows {
                    out.push(merge(l, r));
                }
            }
            _ => out.push(pad_with_nulls(r, &left_columns)),
        }
    }
    out
}

pub fn full_outer_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let left_columns = collect_columns(left);
    let right_columns = collect_columns(right);
    let left_index = build_index(left, &left_key);
    let right_index = build_index(right, &right_key);
    let mut matched_right: HashMap<JoinKey, bool> = HashMap::with_capacity(right_index.len());
    let mut out: Vec<ResultRow> = Vec::new();
    for l in left {
        let key = left_key(l);
        let matches = key.as_ref().and_then(|k| right_index.get(k));
        match matches {
            Some(rows) if !rows.is_empty() => {
                if let Some(k) = key.clone() {
                    matched_right.insert(k, true);
                }
                for r in rows {
                    out.push(merge(l, r));
                }
            }
            _ => out.push(pad_with_nulls(l, &right_columns)),
        }
    }
    for r in right {
        let key = right_key(r);
        if let Some(k) = key.as_ref() {
            if matched_right.contains_key(k) {
                continue;
            }
            if left_index.contains_key(k) {
                continue;
            }
        }
        out.push(pad_with_nulls(r, &left_columns));
    }
    out
}

// -------------------------------------------------------------------------
// Semi / Anti
// -------------------------------------------------------------------------

pub fn semi_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let index = build_index(right, &right_key);
    let mut out: Vec<ResultRow> = Vec::new();
    for l in left {
        if let Some(key) = left_key(l) {
            if index.get(&key).is_some_and(|matches| !matches.is_empty()) {
                out.push(l.clone());
            }
        }
    }
    out
}

pub fn anti_join<L, R>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
    R: Fn(&ResultRow) -> Option<JoinKey>,
{
    let index = build_index(right, &right_key);
    let mut out: Vec<ResultRow> = Vec::new();
    for l in left {
        let matched = match left_key(l) {
            Some(key) => index.get(&key).is_some_and(|matches| !matches.is_empty()),
            None => false,
        };
        if !matched {
            out.push(l.clone());
        }
    }
    out
}

// -------------------------------------------------------------------------
// Cross
// -------------------------------------------------------------------------

pub fn cross_join(left: &[ResultRow], right: &[ResultRow]) -> Vec<ResultRow> {
    let mut out = Vec::with_capacity(left.len().saturating_mul(right.len()));
    for l in left {
        for r in right {
            out.push(merge(l, r));
        }
    }
    out
}

// -------------------------------------------------------------------------
// Sort merge
// -------------------------------------------------------------------------

fn key_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Equal,
    }
}

/// Sort-merge inner join. The caller supplies the column projections
/// to extract the join key from each side; ties on the merge axis
/// produce a Cartesian product of the equal-key blocks (textbook
/// O(n+m) plus the size of the equal blocks).
pub fn sort_merge_inner_join(
    left: &[ResultRow],
    right: &[ResultRow],
    left_col: &str,
    right_col: &str,
) -> Vec<ResultRow> {
    let mut left_sorted: Vec<&ResultRow> = left.iter().collect();
    let mut right_sorted: Vec<&ResultRow> = right.iter().collect();
    left_sorted.sort_by(|a, b| {
        key_cmp(
            a.get(left_col).unwrap_or(&Value::Null),
            b.get(left_col).unwrap_or(&Value::Null),
        )
    });
    right_sorted.sort_by(|a, b| {
        key_cmp(
            a.get(right_col).unwrap_or(&Value::Null),
            b.get(right_col).unwrap_or(&Value::Null),
        )
    });

    let mut out: Vec<ResultRow> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < left_sorted.len() && j < right_sorted.len() {
        let lk = left_sorted[i].get(left_col).unwrap_or(&Value::Null);
        let rk = right_sorted[j].get(right_col).unwrap_or(&Value::Null);
        match key_cmp(lk, rk) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let key = lk.clone();
                let mut li = i;
                while li < left_sorted.len()
                    && key_cmp(left_sorted[li].get(left_col).unwrap_or(&Value::Null), &key)
                        == std::cmp::Ordering::Equal
                {
                    li += 1;
                }
                let mut rj = j;
                while rj < right_sorted.len()
                    && key_cmp(
                        right_sorted[rj].get(right_col).unwrap_or(&Value::Null),
                        &key,
                    ) == std::cmp::Ordering::Equal
                {
                    rj += 1;
                }
                for l in &left_sorted[i..li] {
                    for r in &right_sorted[j..rj] {
                        out.push(merge(l, r));
                    }
                }
                i = li;
                j = rj;
            }
        }
    }
    out
}

// -------------------------------------------------------------------------
// Nested loop
// -------------------------------------------------------------------------

/// Nested-loop join for arbitrary `(left, right) -> bool` predicates.
/// Slow (`O(left * right)`) but the only correct fallback for
/// non-equijoin shapes (range joins, complex theta predicates, etc.).
pub fn nested_loop_join<P>(left: &[ResultRow], right: &[ResultRow], predicate: P) -> Vec<ResultRow>
where
    P: Fn(&ResultRow, &ResultRow) -> bool,
{
    let mut out = Vec::new();
    for l in left {
        for r in right {
            if predicate(l, r) {
                out.push(merge(l, r));
            }
        }
    }
    out
}

// -------------------------------------------------------------------------
// Index-backed
// -------------------------------------------------------------------------

/// Index-backed inner join. The right side exposes its rows under a
/// pre-built hash index; the join probes once per left row in
/// amortised `O(1)`. Used by the planner whenever a btree / hash index
/// already exists on the right-side join column and the right
/// relation is not already in row form.
pub fn index_inner_join<L>(
    left: &[ResultRow],
    right_index: &HashMap<JoinKey, Vec<ResultRow>>,
    left_key: L,
) -> Vec<ResultRow>
where
    L: Fn(&ResultRow) -> Option<JoinKey>,
{
    let mut out = Vec::with_capacity(left.len());
    for l in left {
        if let Some(key) = left_key(l) {
            if let Some(rows) = right_index.get(&key) {
                for r in rows {
                    out.push(merge(l, r));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;

    fn row<const N: usize>(pairs: [(&str, Value); N]) -> ResultRow {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn hash_inner_basic() {
        let l = vec![
            row([("id", Value::Int(1)), ("a", Value::Str("a1".into()))]),
            row([("id", Value::Int(2)), ("a", Value::Str("a2".into()))]),
        ];
        let r = vec![
            row([("uid", Value::Int(2)), ("b", Value::Str("b2".into()))]),
            row([("uid", Value::Int(3)), ("b", Value::Str("b3".into()))]),
        ];
        let out = hash_inner_join(
            &l,
            &r,
            |row| row.get("id").map(JoinKey::new),
            |row| row.get("uid").map(JoinKey::new),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], Value::Int(2));
        assert_eq!(out[0]["b"], Value::Str("b2".into()));
    }

    #[test]
    fn left_outer_pads_unmatched() {
        let l = vec![
            row([("id", Value::Int(1)), ("name", Value::Str("a".into()))]),
            row([("id", Value::Int(2)), ("name", Value::Str("b".into()))]),
        ];
        let r = vec![row([("uid", Value::Int(2)), ("v", Value::Int(99))])];
        let out = left_outer_join(
            &l,
            &r,
            |row| row.get("id").map(JoinKey::new),
            |row| row.get("uid").map(JoinKey::new),
        );
        assert_eq!(out.len(), 2);
        let unmatched = out.iter().find(|r| r["id"] == Value::Int(1)).unwrap();
        assert_eq!(unmatched["v"], Value::Null);
    }

    #[test]
    fn full_outer_matches_unmatched_on_both_sides() {
        let l = vec![row([("id", Value::Int(1))])];
        let r = vec![row([("uid", Value::Int(2))])];
        let out = full_outer_join(
            &l,
            &r,
            |row| row.get("id").map(JoinKey::new),
            |row| row.get("uid").map(JoinKey::new),
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn semi_anti_partition_input() {
        let l = vec![
            row([("id", Value::Int(1))]),
            row([("id", Value::Int(2))]),
            row([("id", Value::Int(3))]),
        ];
        let r = vec![row([("uid", Value::Int(2))]), row([("uid", Value::Int(3))])];
        let semi = semi_join(
            &l,
            &r,
            |row| row.get("id").map(JoinKey::new),
            |row| row.get("uid").map(JoinKey::new),
        );
        let anti = anti_join(
            &l,
            &r,
            |row| row.get("id").map(JoinKey::new),
            |row| row.get("uid").map(JoinKey::new),
        );
        assert_eq!(semi.len(), 2);
        assert_eq!(anti.len(), 1);
        assert_eq!(anti[0]["id"], Value::Int(1));
    }

    #[test]
    fn cross_size_is_product() {
        let l = vec![row([("id", Value::Int(1))]), row([("id", Value::Int(2))])];
        let r = vec![
            row([("uid", Value::Int(10))]),
            row([("uid", Value::Int(20))]),
            row([("uid", Value::Int(30))]),
        ];
        assert_eq!(cross_join(&l, &r).len(), 6);
    }

    #[test]
    fn sort_merge_matches_hash_join() {
        let l = vec![
            row([("k", Value::Int(1)), ("a", Value::Int(10))]),
            row([("k", Value::Int(2)), ("a", Value::Int(20))]),
            row([("k", Value::Int(2)), ("a", Value::Int(21))]),
        ];
        let r = vec![
            row([("k", Value::Int(2)), ("b", Value::Int(200))]),
            row([("k", Value::Int(3)), ("b", Value::Int(300))]),
        ];
        let out = sort_merge_inner_join(&l, &r, "k", "k");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn index_inner_uses_prebuilt_hash() {
        let l = vec![row([("id", Value::Int(7))])];
        let mut idx: HashMap<JoinKey, Vec<ResultRow>> = HashMap::new();
        idx.insert(
            JoinKey::new(&Value::Int(7)),
            vec![row([
                ("uid", Value::Int(7)),
                ("name", Value::Str("x".into())),
            ])],
        );
        let out = index_inner_join(&l, &idx, |row| row.get("id").map(JoinKey::new));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], Value::Str("x".into()));
    }
}
