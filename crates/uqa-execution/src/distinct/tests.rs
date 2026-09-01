//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::NamedTempFile;
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_sql::ResultRow;

use super::encoding::MICROS_PER_DAY;
use super::*;
use crate::physical::run_to_rows;
use crate::scan::TableScan;
use crate::{ExpressionEvaluator, PhysicalRow, ScalarEvalContext};

fn row(a: i64, b: i64) -> ResultRow {
    [("a".into(), Value::Int(a)), ("b".into(), Value::Int(b))]
        .into_iter()
        .collect()
}

fn value_row(value: Value) -> ResultRow {
    [("v".into(), value)].into_iter().collect()
}

struct Evaluator;

impl ExpressionEvaluator for Evaluator {
    fn evaluate(
        &self,
        expression: &ScalarExpr,
        row: &dyn uqa_sql::expr::RowLookup,
    ) -> ExecResult<Value> {
        Ok(crate::eval_scalar(
            expression,
            &ScalarEvalContext::from_row_lookup(row, &[]),
        )?)
    }
}

#[test]
fn all_columns_and_distinct_on_preserve_the_first_row() {
    let rows = vec![row(1, 10), row(1, 10), row(1, 11), row(2, 20)];
    let scan = TableScan::from_rows(vec!["a".into(), "b".into()], rows.clone());
    let mut all = Distinct::all_with_work_mem(Box::new(scan), 1);
    let (_, all_rows) = run_to_rows(&mut all).unwrap();
    assert_eq!(all_rows, vec![row(1, 10), row(1, 11), row(2, 20)]);

    let scan = TableScan::from_rows(vec!["a".into(), "b".into()], rows);
    let mut on = Distinct::on_with_work_mem(
        Box::new(scan),
        vec![ScalarExpr::Column("a".into())],
        Arc::new(Evaluator),
        1,
    );
    let (_, on_rows) = run_to_rows(&mut on).unwrap();
    assert_eq!(on_rows, vec![row(1, 10), row(2, 20)]);
}

#[test]
fn distinct_is_a_row_lock_identity_barrier() {
    let schema = RowSchema::new(vec!["v".into()]);
    let row = PhysicalRow::from_values(vec![Value::Int(1)])
        .with_lock_origin(crate::RowLockOrigin::new("source", "public.source", 1));
    let scan = TableScan::from_physical_rows(schema, vec![row]);
    let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 1);

    let batches = crate::physical::run_to_batches(&mut distinct).unwrap();
    assert!(batches[0].rows[0].lock_origins().is_empty());
}

#[test]
fn tiny_budget_migrates_to_disk_and_never_retains_key_bytes() {
    let rows: Vec<_> = (0..50)
        .flat_map(|value| [value_row(Value::Int(value)), value_row(Value::Int(value))])
        .collect();
    let scan = TableScan::from_rows(vec!["v".into()], rows);
    let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 1);
    distinct.open().unwrap();
    let output = distinct.next().unwrap().unwrap();
    assert_eq!(output.rows.len(), 50);
    assert!(distinct.has_spilled());
    assert_eq!(distinct.in_memory_key_bytes(), 0);
    assert!(distinct.next().unwrap().is_none());
    distinct.close().unwrap();
}

#[test]
fn exact_row_set_persists_disk_backed_state_across_fixpoint_phases() {
    let schema = vec!["a".into(), "b".into()];
    let mut seen = ExactRowSet::new(1);
    for value in 0..100 {
        assert!(seen.insert_row(&row(value, value + 1), &schema).unwrap());
    }
    assert!(seen.has_spilled());
    assert_eq!(seen.in_memory_key_bytes(), 0);
    for value in 0..100 {
        assert!(seen.contains_row(&row(value, value + 1), &schema).unwrap());
        assert!(!seen.insert_row(&row(value, value + 1), &schema).unwrap());
    }
    assert!(!seen.contains_row(&row(101, 102), &schema).unwrap());
}

#[test]
fn binary_keys_cover_every_value_variant_without_structural_collisions() {
    let one = DecimalValue::parse("1.000").unwrap();
    let mut nested_map = BTreeMap::new();
    nested_map.insert("x".into(), Value::Float(1.0));
    let values = vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        Value::Decimal(one),
        Value::Float(f64::NAN),
        Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
        Value::Str("a\0b".into()),
        Value::Bytes(vec![b'a', 0, b'b']),
        Value::Temporal(TemporalValue::Date { days: 1 }),
        Value::Temporal(TemporalValue::Time {
            micros: MICROS_PER_DAY as i64 + 7,
        }),
        Value::Temporal(TemporalValue::Time { micros: 7 }),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 3_600_000_000,
            offset_minutes: 60,
        }),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 0,
            offset_minutes: 0,
        }),
        Value::Temporal(TemporalValue::Timestamp { micros: 9 }),
        Value::Temporal(TemporalValue::TimestampTz { micros: 9 }),
        Value::Temporal(TemporalValue::Interval {
            months: 1,
            days: 0,
            micros: 0,
        }),
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 30,
            micros: 0,
        }),
        Value::List(vec![Value::Int(1), Value::Str("x".into())]),
        Value::List(vec![Value::Float(1.0), Value::Str("x".into())]),
        Value::Map(nested_map),
    ];
    let rows: Vec<_> = values
        .iter()
        .cloned()
        .chain(values.iter().cloned())
        .map(value_row)
        .collect();
    let scan = TableScan::from_rows(vec!["v".into()], rows);
    let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 0);
    let (_, output) = run_to_rows(&mut distinct).unwrap();

    // true/int/float/decimal share one numeric key; NaN payloads share one;
    // normalized time/time-tz/interval pairs and nested numeric values do
    // likewise. The string and byte representations stay distinct.
    assert_eq!(output.len(), 15);
    assert_eq!(output[1], value_row(Value::Bool(true)));
    assert!(matches!(output[2].get("v"), Some(Value::Float(v)) if v.is_nan()));
    assert_eq!(output[5], value_row(Value::Str("a\0b".into())));
    assert_eq!(output[6], value_row(Value::Bytes(vec![b'a', 0, b'b'])));
}

#[test]
fn physical_numeric_join_key_stays_inline_and_matches_distinct_encoding() {
    let value = Value::Int(42);
    let key = encode_non_null_key(std::iter::once(Some(&value)))
        .unwrap()
        .unwrap();
    assert!(!key.spilled());
    assert_eq!(
        key.as_slice(),
        encode_key(std::slice::from_ref(&value)).unwrap()
    );

    let null = Value::Null;
    assert!(encode_non_null_key(std::iter::once(Some(&null)))
        .unwrap()
        .is_none());
}

#[test]
fn canonical_row_hash_streams_borrowed_composites_with_sql_equality() {
    let integer = Value::Int(1);
    let decimal = Value::Decimal(DecimalValue::parse("1.000").unwrap());
    let text = Value::Str("group".into());
    let hash_state = ahash::RandomState::new();
    assert_eq!(
        hash_canonical_row(&hash_state, [Some(&integer), Some(&text)].into_iter()).unwrap(),
        hash_canonical_row(&hash_state, [Some(&decimal), Some(&text)].into_iter()).unwrap()
    );

    let null = Value::Null;
    assert_eq!(
        hash_canonical_row(&hash_state, std::iter::once(None)).unwrap(),
        hash_canonical_row(&hash_state, std::iter::once(Some(&null))).unwrap()
    );
}

#[test]
fn compact_text_pair_key_is_stable_for_borrowed_and_owned_values() {
    let first = Value::Str("A".into());
    let second = Value::Str("O".into());
    let values = [Some(&first), Some(&second)];

    let key = try_pack_compact_text_pair(values.into_iter()).unwrap();
    let owned = [first.clone(), second.clone()];
    assert_eq!(
        key,
        try_pack_compact_text_pair(owned.iter().map(Some)).unwrap(),
    );
    let long = Value::Str("x".repeat(64));
    assert_eq!(
        try_pack_compact_text_pair([Some(&long), Some(&second)].into_iter()),
        None,
    );

    let candidates = [
        Value::Null,
        Value::Str(String::new()),
        Value::Str("A".into()),
        Value::Str("AB".into()),
        Value::Str("ABC".into()),
        Value::Str("é".into()),
        Value::Str("한".into()),
    ];
    let mut keys = std::collections::BTreeSet::new();
    for first in &candidates {
        for second in &candidates {
            assert!(keys.insert(
                try_pack_compact_text_pair([Some(first), Some(second)].into_iter()).unwrap()
            ));
        }
    }
}

#[test]
fn canonical_row_hash_set_copies_only_new_keys_and_probes_borrowed() {
    let one = Value::Int(1);
    let two = Value::Int(2);
    let decimal_one = Value::Decimal(DecimalValue::parse("1.000").unwrap());
    let mut rows = CanonicalRowHashSet::new();

    assert!(rows.insert_borrowed(&[&one, &two]).unwrap());
    assert!(!rows.insert_borrowed(&[&decimal_one, &two]).unwrap());
    assert!(rows.contains_borrowed(&[&decimal_one, &two]).unwrap());
    assert!(!rows.contains_borrowed(&[&two, &one]).unwrap());
    assert_eq!(rows.rows.len(), 1);
    assert!(!rows.rows[0].spilled());
}

#[test]
fn temporary_directory_is_removed_on_drop() {
    let parent = tempfile::tempdir().unwrap();
    let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
    let mut distinct =
        Distinct::all_with_work_mem(Box::new(scan), 0).with_spill_directory(parent.path());
    distinct.open().unwrap();
    distinct.next().unwrap();
    let spill_path = distinct.spill_path().unwrap().to_path_buf();
    assert!(spill_path.exists());
    drop(distinct);
    assert!(!spill_path.exists());
}

#[test]
fn spill_creation_failure_is_returned() {
    let not_a_directory = NamedTempFile::new().unwrap();
    let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
    let mut distinct =
        Distinct::all_with_work_mem(Box::new(scan), 0).with_spill_directory(not_a_directory.path());
    let error = run_to_rows(&mut distinct).unwrap_err();
    assert!(error
        .to_string()
        .contains("failed to create DISTINCT spill directory"));
}

#[test]
fn truncated_disk_record_is_reported() {
    let first = encode_key(&[Value::Int(1)]).unwrap();
    let bucket = stable_hash(&first) % DISK_BUCKETS;
    let second = (2..10_000)
        .map(|value| encode_key(&[Value::Int(value)]).unwrap())
        .find(|key| stable_hash(key) % DISK_BUCKETS == bucket)
        .unwrap();
    let mut set = SeenKeySet::new(0, None);
    assert!(set.insert(first).unwrap());
    let disk = set.disk.as_mut().unwrap();
    let file = disk.buckets.get_mut(&(bucket as u8)).unwrap();
    file.set_len(4).unwrap();
    let error = set.insert(second).unwrap_err();
    assert!(error
        .to_string()
        .contains("truncated DISTINCT spill key length"));
}

struct FailingEvaluator;

struct MismatchedSchemaScan {
    declared: RowSchema,
    emitted: Option<Batch>,
}

impl PhysicalOperator for MismatchedSchemaScan {
    fn row_schema(&self) -> &RowSchema {
        &self.declared
    }

    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        Ok(self.emitted.take())
    }

    fn close(&mut self) -> ExecResult<()> {
        Ok(())
    }
}

impl ExpressionEvaluator for FailingEvaluator {
    fn evaluate(
        &self,
        _expression: &ScalarExpr,
        _row: &dyn uqa_sql::expr::RowLookup,
    ) -> ExecResult<Value> {
        Err(ExecError::Other("intentional evaluator failure".into()))
    }
}

#[test]
fn evaluator_errors_are_propagated() {
    let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
    let mut distinct = Distinct::on_with_work_mem(
        Box::new(scan),
        vec![ScalarExpr::Column("v".into())],
        Arc::new(FailingEvaluator),
        0,
    );
    let error = run_to_rows(&mut distinct).unwrap_err();
    assert!(error.to_string().contains("intentional evaluator failure"));
}

#[test]
fn distinct_rejects_a_child_batch_with_a_different_schema() {
    let scan = MismatchedSchemaScan {
        declared: RowSchema::new(vec!["declared".into()]),
        emitted: Some(Batch::from_physical_rows(
            RowSchema::new(vec!["actual".into()]),
            vec![PhysicalRow::from_values(vec![Value::Int(1)])],
        )),
    };
    let mut distinct = Distinct::all(Box::new(scan));

    let error = run_to_rows(&mut distinct).unwrap_err();

    assert!(error.to_string().contains("DISTINCT input schema mismatch"));
}
