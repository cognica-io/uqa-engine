//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use super::*;
use crate::physical::run_to_rows;
use crate::scan::TableScan;

fn row(values: &[(&str, Value)]) -> ResultRow {
    values
        .iter()
        .map(|(column, value)| ((*column).to_string(), value.clone()))
        .collect()
}

fn evaluator() -> SharedExpressionEvaluator<'static> {
    Arc::new(TestEvaluator)
}

#[test]
fn disk_hash_index_rejects_corrupt_key_length_without_allocating_it() {
    let key = b"join-key";
    let mut index = DiskHashIndex::new(None).unwrap();
    index.insert(key, 7).unwrap();
    let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS).unwrap();
    let file = index.buckets.get_mut(&bucket).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&(1_u64 << 40).to_le_bytes()).unwrap();
    file.flush().unwrap();

    let error = index.for_each_match(key, &mut |_| Ok(())).unwrap_err();
    assert!(error
        .to_string()
        .contains("exceeds remaining bucket record bytes"));
}

struct TestEvaluator;

impl crate::ExpressionEvaluator for TestEvaluator {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
        Ok(crate::eval_scalar(
            expression,
            &crate::ScalarEvalContext::new(Some(row), &[]),
        )?)
    }
}

#[test]
fn hash_full_join_preserves_unmatched_rows() {
    let left = TableScan::from_rows(
        vec!["l.id".into()],
        vec![
            row(&[("l.id", Value::Int(1))]),
            row(&[("l.id", Value::Int(2))]),
        ],
    );
    let right = TableScan::from_rows(
        vec!["r.id".into()],
        vec![
            row(&[("r.id", Value::Int(2))]),
            row(&[("r.id", Value::Int(3))]),
        ],
    );
    let mut join = HashJoin::new(
        Box::new(left),
        Box::new(right),
        JoinKind::Full,
        vec![ScalarExpr::Column("l.id".into())],
        vec![ScalarExpr::Column("r.id".into())],
        evaluator(),
        row(&[("l.id", Value::Null)]),
        row(&[("r.id", Value::Null)]),
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows
        .iter()
        .any(|row| row["l.id"] == Value::Int(1) && row["r.id"] == Value::Null));
    assert!(rows
        .iter()
        .any(|row| row["l.id"] == Value::Int(2) && row["r.id"] == Value::Int(2)));
    assert!(rows
        .iter()
        .any(|row| row["l.id"] == Value::Null && row["r.id"] == Value::Int(3)));
}

#[test]
fn hash_join_applies_residual_predicate_before_marking_matches() {
    let left = TableScan::from_rows(
        vec!["l.k".into(), "l.v".into()],
        vec![
            row(&[("l.k", Value::Int(1)), ("l.v", Value::Int(1))]),
            row(&[("l.k", Value::Int(1)), ("l.v", Value::Int(3))]),
        ],
    );
    let right = TableScan::from_rows(
        vec!["r.k".into(), "r.v".into()],
        vec![row(&[("r.k", Value::Int(1)), ("r.v", Value::Int(2))])],
    );
    let predicate = ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Greater,
        lhs: Box::new(ScalarExpr::Column("l.v".into())),
        rhs: Box::new(ScalarExpr::Column("r.v".into())),
    };
    let mut join = HashJoin::new_with_work_mem_and_predicate(
        Box::new(left),
        Box::new(right),
        JoinKind::Full,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        Some(predicate),
        evaluator(),
        row(&[("l.k", Value::Null), ("l.v", Value::Null)]),
        row(&[("r.k", Value::Null), ("r.v", Value::Null)]),
        1,
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| {
        row["l.v"] == Value::Int(1) && row["r.k"] == Value::Null && row["r.v"] == Value::Null
    }));
    assert!(rows
        .iter()
        .any(|row| row["l.v"] == Value::Int(3) && row["r.v"] == Value::Int(2)));
}

#[test]
fn nested_loop_predicate_errors_propagate() {
    struct FailingEvaluator;
    impl crate::ExpressionEvaluator for FailingEvaluator {
        fn evaluate(&self, _: &ScalarExpr, _: &ResultRow) -> ExecResult<Value> {
            Err(crate::ExecError::Other("join predicate failed".into()))
        }
    }
    let left = TableScan::from_rows(vec!["l".into()], vec![row(&[("l", Value::Int(1))])]);
    let right = TableScan::from_rows(vec!["r".into()], vec![row(&[("r", Value::Int(1))])]);
    let mut join = NestedLoopJoin::new(
        Box::new(left),
        Box::new(right),
        JoinKind::Inner,
        Some(ScalarExpr::Literal(Value::Bool(true))),
        Arc::new(FailingEvaluator),
        ResultRow::new(),
        ResultRow::new(),
    );
    let error = run_to_rows(&mut join).unwrap_err();
    assert!(error.to_string().contains("join predicate failed"));
}

#[test]
fn high_cardinality_hash_join_spills_output() {
    let left = TableScan::from_rows(
        vec!["l.k".into(), "l.id".into()],
        (0..48)
            .map(|id| row(&[("l.k", Value::Int(1)), ("l.id", Value::Int(id))]))
            .collect(),
    );
    let right = TableScan::from_rows(
        vec!["r.k".into(), "r.id".into()],
        (0..48)
            .map(|id| row(&[("r.k", Value::Int(1)), ("r.id", Value::Int(id))]))
            .collect(),
    );
    let mut join = HashJoin::new_with_work_mem(
        Box::new(left),
        Box::new(right),
        JoinKind::Inner,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        evaluator(),
        ResultRow::new(),
        ResultRow::new(),
        1,
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    assert!(join.output_has_spilled());
    assert!(join.right_input_has_spilled());
    assert!(join.hash_index_has_spilled());
    assert_eq!(rows.len(), 48 * 48);
}

#[test]
fn high_cardinality_nested_loop_join_spills_output() {
    let left = TableScan::from_rows(
        vec!["l.id".into()],
        (0..48).map(|id| row(&[("l.id", Value::Int(id))])).collect(),
    );
    let right = TableScan::from_rows(
        vec!["r.id".into()],
        (0..48).map(|id| row(&[("r.id", Value::Int(id))])).collect(),
    );
    let mut join = NestedLoopJoin::new_with_work_mem(
        Box::new(left),
        Box::new(right),
        JoinKind::Cross,
        None,
        evaluator(),
        ResultRow::new(),
        ResultRow::new(),
        1,
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    assert!(join.output_has_spilled());
    assert!(join.right_input_has_spilled());
    assert_eq!(rows.len(), 48 * 48);
}

struct GeneratedRows {
    schema: Vec<String>,
    prefix: &'static str,
    next: i64,
    end: i64,
}

impl crate::RowSource for GeneratedRows {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        if self.next == self.end {
            return Ok(None);
        }
        let value = self.next;
        self.next += 1;
        Ok(Some(row(&[(self.prefix, Value::Int(value))])))
    }
}

#[test]
fn tiny_work_mem_hash_join_streams_left_and_spills_distinct_build_keys() {
    const ROWS: i64 = 2_048;
    let left = TableScan::new(Box::new(GeneratedRows {
        schema: vec!["l.k".into()],
        prefix: "l.k",
        next: 0,
        end: ROWS,
    }));
    let right = TableScan::new(Box::new(GeneratedRows {
        schema: vec!["r.k".into()],
        prefix: "r.k",
        next: 0,
        end: ROWS,
    }));
    let mut join = HashJoin::new_with_work_mem(
        Box::new(left),
        Box::new(right),
        JoinKind::Inner,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        evaluator(),
        ResultRow::new(),
        ResultRow::new(),
        1,
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    assert_eq!(rows.len(), ROWS as usize);
    assert!(join.right_input_has_spilled());
    assert!(join.hash_index_has_spilled());
    assert!(join.output_has_spilled());
}

struct FailingRows {
    schema: Vec<String>,
    emitted: bool,
}

impl crate::RowSource for FailingRows {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        if self.emitted {
            return Err(ExecError::Other("injected join input failure".into()));
        }
        self.emitted = true;
        Ok(Some(row(&[("r.k", Value::Int(1))])))
    }
}

#[test]
fn build_side_input_error_is_propagated_before_any_join_result() {
    let left = TableScan::from_rows(vec!["l.k".into()], vec![row(&[("l.k", Value::Int(1))])]);
    let right = TableScan::new(Box::new(FailingRows {
        schema: vec!["r.k".into()],
        emitted: false,
    }));
    let mut join = HashJoin::new_with_work_mem(
        Box::new(left),
        Box::new(right),
        JoinKind::Inner,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        evaluator(),
        ResultRow::new(),
        ResultRow::new(),
        1,
    );
    let error = run_to_rows(&mut join).unwrap_err();
    assert!(error.to_string().contains("injected join input failure"));
}
