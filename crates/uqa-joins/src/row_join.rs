//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational joins over `ResultRow` row sets.
//!
//! Implements inner, outer, semi, cross, sort-merge, and index-assisted
//! joins. The implementation operates on `ResultRow`
//! (`BTreeMap<String, Value>`) directly so it
//! can be plugged into the engine's row-tuple SQL pipeline without an
//! extra adapter; the returned `Vec<ResultRow>` carries qualifier-
//! prefixed columns that the projection layer consumes verbatim.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use uqa_core::{DecimalValue, TemporalValue, Value};
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

/// A hashable join key whose equality is exactly [`Value::cmp`].
///
/// In particular, SQL numeric keys compare by value across bool, integer,
/// float, and decimal representations. Keeping the original `Value` avoids
/// making hash joins disagree with sort-merge joins for pairs such as
/// `1`, `1.0`, and `DECIMAL '1.00'`.
#[derive(Debug, Clone)]
pub struct JoinKey(Value);

impl JoinKey {
    pub fn new(value: &Value) -> Self {
        Self(value.clone())
    }

    /// Composite key for multi-column equijoins.
    pub fn composite(values: &[&Value]) -> Self {
        Self(Value::List(
            values.iter().map(|value| (*value).clone()).collect(),
        ))
    }
}

impl PartialEq for JoinKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for JoinKey {}

impl PartialOrd for JoinKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JoinKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hash for JoinKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_value(&self.0, state);
    }
}

fn hash_value<H: Hasher>(value: &Value, state: &mut H) {
    match value {
        Value::Datum(datum) => {
            16_u8.hash(state);
            datum.type_oid().hash(state);
            datum.offset().hash(state);
            datum.bytes().hash(state);
        }
        Value::Null => 0_u8.hash(state),
        Value::Void => 13_u8.hash(state),
        Value::Bool(value) => hash_decimal_numeric(&DecimalValue::from_bool(*value), state),
        Value::Int(value) => hash_decimal_numeric(&DecimalValue::from_i64(*value), state),
        Value::Float(value) => hash_float_numeric(*value, state),
        Value::Decimal(value) => hash_decimal_numeric(value, state),
        Value::Str(value) => {
            2_u8.hash(state);
            value.hash(state);
        }
        Value::FixedChar(value) => {
            7_u8.hash(state);
            value.trim_end_matches(' ').hash(state);
        }
        Value::Bytes(value) => {
            3_u8.hash(state);
            value.hash(state);
        }
        Value::BitString(value) => {
            17_u8.hash(state);
            value.hash(state);
        }
        Value::Temporal(value) => hash_temporal(value, state),
        Value::Json(value) => {
            8_u8.hash(state);
            value.hash(state);
        }
        Value::JsonB(value) => {
            9_u8.hash(state);
            if let Some(key) = uqa_core::jsonb_equality_key(value) {
                key.hash(state);
            } else {
                value.hash(state);
            }
        }
        Value::Array(array) => {
            12_u8.hash(state);
            array.lower_bounds().hash(state);
            array.elements().len().hash(state);
            for value in array.elements() {
                hash_value(value, state);
            }
        }
        Value::LegacyVector(vector) => {
            14_u8.hash(state);
            vector
                .write_comparison_key(|bytes| {
                    state.write(bytes);
                    Ok::<_, std::convert::Infallible>(())
                })
                .expect("hash sink cannot fail");
        }
        Value::List(values) => {
            5_u8.hash(state);
            values.len().hash(state);
            for value in values {
                hash_value(value, state);
            }
        }
        Value::Row(values) => {
            10_u8.hash(state);
            values.len().hash(state);
            for value in values {
                hash_value(value, state);
            }
        }
        Value::Record(fields) => {
            11_u8.hash(state);
            fields.len().hash(state);
            for (_, value) in fields {
                hash_value(value, state);
            }
        }
        Value::Map(values) => {
            6_u8.hash(state);
            values.len().hash(state);
            for (key, value) in values {
                key.hash(state);
                hash_value(value, state);
            }
        }
        // Equal enum values share their type OID and immutable label key.
        Value::Enum(value) => {
            15_u8.hash(state);
            value.hash(state);
        }
    }
}

fn hash_decimal_numeric<H: Hasher>(value: &DecimalValue, state: &mut H) {
    if value.is_nan() {
        7_u8.hash(state);
    } else if value.is_positive_infinity() {
        8_u8.hash(state);
    } else if value.is_negative_infinity() {
        9_u8.hash(state);
    } else {
        1_u8.hash(state);
        value.to_canonical_string().hash(state);
    }
}

fn hash_float_numeric<H: Hasher>(value: f64, state: &mut H) {
    hash_decimal_numeric(&DecimalValue::from_f64_exact(value), state);
}

fn hash_temporal<H: Hasher>(value: &TemporalValue, state: &mut H) {
    const MICROS_PER_DAY: i128 = 86_400_000_000;
    4_u8.hash(state);
    match value {
        TemporalValue::Date { days } => (0_u8, i128::from(*days)).hash(state),
        TemporalValue::Time { micros } => {
            (1_u8, i128::from(*micros).rem_euclid(MICROS_PER_DAY)).hash(state);
        }
        TemporalValue::TimeTz {
            micros,
            offset_seconds,
        } => {
            let normalized = (i128::from(*micros) - i128::from(*offset_seconds) * 1_000_000)
                .rem_euclid(MICROS_PER_DAY);
            (2_u8, normalized).hash(state);
        }
        TemporalValue::Timestamp {
            postgres_micros: micros,
        } => (3_u8, i128::from(*micros)).hash(state),
        TemporalValue::TimestampTz {
            postgres_micros: micros,
        } => (4_u8, i128::from(*micros)).hash(state),
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            let flattened = (i128::from(*months) * 30 + i128::from(*days)) * MICROS_PER_DAY
                + i128::from(*micros);
            (5_u8, flattened).hash(state);
        }
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

/// Error-preserving hash inner join. This is the physical form used when join
/// keys are expressions rather than direct columns: key evaluation failures
/// must abort the query instead of being reinterpreted as SQL NULL.
pub fn try_hash_inner_join<L, R, E>(
    left: &[ResultRow],
    right: &[ResultRow],
    left_key: L,
    right_key: R,
) -> Result<Vec<ResultRow>, E>
where
    L: Fn(&ResultRow) -> Result<Option<JoinKey>, E>,
    R: Fn(&ResultRow) -> Result<Option<JoinKey>, E>,
{
    let (build_rows, probe_rows, build_is_left) = if left.len() <= right.len() {
        (left, right, true)
    } else {
        (right, left, false)
    };
    let build_key_fn: &dyn Fn(&ResultRow) -> Result<Option<JoinKey>, E> =
        if build_is_left { &left_key } else { &right_key };
    let probe_key_fn: &dyn Fn(&ResultRow) -> Result<Option<JoinKey>, E> =
        if build_is_left { &right_key } else { &left_key };
    let mut index: HashMap<JoinKey, Vec<&ResultRow>> = HashMap::with_capacity(build_rows.len());
    for row in build_rows {
        if let Some(key) = build_key_fn(row)? {
            index.entry(key).or_default().push(row);
        }
    }
    let mut out = Vec::with_capacity(probe_rows.len());
    for probe in probe_rows {
        let Some(key) = probe_key_fn(probe)? else {
            continue;
        };
        if let Some(matches) = index.get(&key) {
            for build_row in matches {
                out.push(if build_is_left {
                    merge(build_row, probe)
                } else {
                    merge(probe, build_row)
                });
            }
        }
    }
    Ok(out)
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
    a.cmp(b)
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
    // SQL NULL never equals another value, including NULL. Missing join
    // columns have the same non-match semantics instead of being synthesized
    // into a shared `Value::Null` key.
    let mut left_sorted: Vec<(&Value, &ResultRow)> = left
        .iter()
        .filter_map(|row| {
            row.get(left_col)
                .filter(|value| **value != Value::Null)
                .map(|key| (key, row))
        })
        .collect();
    let mut right_sorted: Vec<(&Value, &ResultRow)> = right
        .iter()
        .filter_map(|row| {
            row.get(right_col)
                .filter(|value| **value != Value::Null)
                .map(|key| (key, row))
        })
        .collect();
    left_sorted.sort_by(|(a, _), (b, _)| key_cmp(a, b));
    right_sorted.sort_by(|(a, _), (b, _)| key_cmp(a, b));

    let mut out: Vec<ResultRow> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < left_sorted.len() && j < right_sorted.len() {
        let lk = left_sorted[i].0;
        let rk = right_sorted[j].0;
        match key_cmp(lk, rk) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let key = lk.clone();
                let mut li = i;
                while li < left_sorted.len()
                    && key_cmp(left_sorted[li].0, &key) == std::cmp::Ordering::Equal
                {
                    li += 1;
                }
                let mut rj = j;
                while rj < right_sorted.len()
                    && key_cmp(right_sorted[rj].0, &key) == std::cmp::Ordering::Equal
                {
                    rj += 1;
                }
                for (_, l) in &left_sorted[i..li] {
                    for (_, r) in &right_sorted[j..rj] {
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
mod tests;
