//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::test_support::NoRoutines,
    spill::{temporary_reads, SpillBatchWriter},
    PhysicalRow, SpillBuffer,
};
use uqa_core::memory::ProductionControl;

fn equality() -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::qualified_column("s", "id")),
        rhs: Box::new(ScalarExpr::qualified_column("t", "id")),
    }
}

fn sources(values: impl IntoIterator<Item = Value>, budget: usize) -> SharedSpill {
    typed_sources(ColumnType::Integer, values, budget)
}

fn typed_sources(
    ty: ColumnType,
    values: impl IntoIterator<Item = Value>,
    budget: usize,
) -> SharedSpill {
    let schema = RowSchema::with_qualified_types(
        "s",
        vec!["id".into(), "data".into()],
        vec![Some(ty), Some(ColumnType::Bytea)],
    );
    let mut buffer = SpillBuffer::new(budget);
    let mut writer = SpillBatchWriter::new(schema.clone()).unwrap();
    for value in values {
        writer
            .push(
                &mut buffer,
                PhysicalRow::from_values(vec![value, Value::Bytes(vec![42; 512])]),
            )
            .unwrap();
    }
    writer.finish(&mut buffer).unwrap();
    buffer.into_shared(schema).unwrap()
}

fn target(value: Value) -> OwnedPhysicalRow {
    let schema =
        RowSchema::with_qualified_types("t", vec!["id".into()], vec![Some(ColumnType::Integer)]);
    OwnedPhysicalRow::new(schema, PhysicalRow::from_values(vec![value]))
}

fn candidates(expression: &ScalarExpr, budget: usize) -> MergeCandidates<'_> {
    MergeCandidates::new(
        expression,
        &[],
        &NoRoutines,
        &NoRoutines,
        budget,
        ProductionControl::uncontrolled().capture(),
    )
}

fn measure(count: i64, budget: usize, sequential: bool) -> (u64, i64) {
    let source = sources(
        (0..count)
            .map(Value::Int)
            .chain([Value::Int(3), Value::Null]),
        budget,
    );
    let condition = if sequential {
        ScalarExpr::And(vec![equality(), ScalarExpr::Literal(Value::Bool(true))])
    } else {
        equality()
    };
    let mut access = candidates(&condition, budget);
    let mut evaluations = 0;
    let reads = temporary_reads(|| {
        for n in (0..count).rev() {
            let mut matched = Vec::new();
            access
                .visit(&target(Value::Int(n)), &source, |ordinal, row| {
                    evaluations += 1;
                    if row.physical_value_at(0) == Some(&Value::Int(n)) {
                        matched.push(ordinal);
                    }
                    Ok(())
                })
                .unwrap();
            let mut expected = vec![n as u64];
            if n == 3 {
                expected.push(count as u64);
            }
            assert_eq!(matched, expected);
        }
    });
    let expected = if sequential {
        count * (count + 2)
    } else {
        2 * count + 2
    };
    assert_eq!(evaluations, expected);
    (reads, evaluations)
}

#[test]
fn merge_equality_bounds_spilled_source_reads_and_preserves_duplicate_order() {
    let small = measure(12, 1, false);
    let medium = measure(48, 1, false);
    let large = measure(192, 1, false);
    let medium_scan = measure(48, 1, true);
    let large_scan = measure(192, 1, true);
    assert!(
        small.0 > 0 && medium.0 <= 7 * small.0 && large.0 <= 7 * medium.0,
        "temporary reads and candidate evaluations at 12, 48 and 192 keys: {small:?}, {medium:?}, {large:?}"
    );
    // At 48 keys the sequential input fits in six encrypted blocks per scan. The index pays for its directory and positional rows even for this small input.
    assert!(
        medium.0 < 2 * medium_scan.0 && large.0 < large_scan.0,
        "indexed and sequential reads: 48 keys {medium:?}/{medium_scan:?}, 192 keys {large:?}/{large_scan:?}"
    );
    assert!(large_scan.0 > 8 * medium_scan.0);
    assert_eq!(measure(48, usize::MAX, false), (0, medium.1));
}

#[test]
fn fallible_merge_keys_and_residuals_preserve_evaluation_order() {
    let sql = equality();
    let mut lookup = candidates(&sql, 128);
    let empty = sources([], 128);
    lookup
        .visit(&target(Value::Int(1)), &empty, |_, _| {
            panic!("empty source")
        })
        .unwrap();
    assert!(matches!(lookup.access, SourceAccess::FirstTarget));
    let source = sources(
        [
            Value::Int(1),
            Value::Str("runtime field".into()),
            Value::Null,
        ],
        128,
    );
    for value in [Value::Null, Value::Int(1)] {
        let mut ordinals = vec![];
        lookup
            .visit(&target(value), &source, |ordinal, _| {
                ordinals.push(ordinal);
                Ok(())
            })
            .unwrap();
        assert_eq!(ordinals, [0, 1, 2]);
    }
    assert!(matches!(lookup.access, SourceAccess::Sequential));
    let residual = ScalarExpr::And(vec![ScalarExpr::Literal(Value::Bool(true)), equality()]);
    let mut lookup = candidates(&residual, 1);
    let source = sources([Value::Int(2), Value::Int(5)], 1);
    lookup
        .visit(&target(Value::Int(0)), &source, |_, _| Ok(()))
        .unwrap();
    let mut calls = 0;
    let e = lookup
        .visit(&target(Value::Int(5)), &source, |ordinal, _| {
            calls += 1;
            assert_eq!(ordinal, 0);
            Err(SQLError::Routine {
                sqlstate: "22012".into(),
                message: "ordered qualification".into(),
            })
        })
        .unwrap_err();
    assert_eq!(e.sqlstate(), Some("22012"));
    assert_eq!(calls, 1);
}

#[test]
fn repeated_merge_probes_agree_with_sql_equality_at_type_boundaries() {
    for (left_type, target_values, right_type, source_values) in [
        (
            ColumnType::BigInteger,
            vec![
                Value::Int(9_007_199_254_740_992),
                Value::Int(9_007_199_254_740_993),
            ],
            ColumnType::DoublePrecision,
            vec![Value::Float(9_007_199_254_740_992.0), Value::Null],
        ),
        (
            ColumnType::DoublePrecision,
            vec![Value::Float(-0.0), Value::Float(f64::NAN), Value::Null],
            ColumnType::DoublePrecision,
            vec![
                Value::Float(0.0),
                Value::Float(-0.0),
                Value::Float(f64::NAN),
                Value::Null,
            ],
        ),
        (
            ColumnType::Bpchar,
            vec![
                Value::FixedChar("ab   ".into()),
                Value::FixedChar("z".into()),
            ],
            ColumnType::Bpchar,
            vec![
                Value::FixedChar("ab".into()),
                Value::FixedChar("ab ".into()),
                Value::Null,
            ],
        ),
    ] {
        let sql = equality();
        for budget in [1, usize::MAX] {
            let source = typed_sources(right_type.clone(), source_values.iter().cloned(), budget);
            let target_schema = RowSchema::with_qualified_types(
                "t",
                vec!["id".into()],
                vec![Some(left_type.clone())],
            );
            let joined_schema = RowSchema::join(&target_schema, source.row_schema(), []);
            let bound = crate::bind_type_introspection_with_resolver(
                sql.clone(),
                &joined_schema,
                &[],
                &NoRoutines,
            );
            let mut access = candidates(&sql, budget);
            // The first visit scans the source. Revisit each value after the eligibility decision.
            for value in target_values.iter().take(1).chain(&target_values) {
                let target = OwnedPhysicalRow::new(
                    target_schema.clone(),
                    PhysicalRow::from_values(vec![value.clone()]),
                );
                let qualifies = |source: &OwnedPhysicalRow| -> Result<bool, SQLError> {
                    let joined = crate::mutation::rows::join_rows(&target, source);
                    let view = joined.view();
                    let value = crate::scalar::plan::eval_physical_scalar(
                        &bound,
                        &[],
                        &crate::scalar::plan::PhysicalEvalContext::from_row_lookup(&view, &[]),
                    )?;
                    uqa_sql::expr::truthy(&value)
                };
                let mut expected = vec![];
                for (ordinal, row) in source.read_rows().unwrap().enumerate() {
                    if qualifies(&row.unwrap()).unwrap() {
                        expected.push(ordinal as u64);
                    }
                }
                let mut actual = vec![];
                access
                    .visit(&target, &source, |ordinal, row| {
                        if qualifies(row)? {
                            actual.push(ordinal);
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(
                    actual, expected,
                    "{left_type:?} = {right_type:?} with {value:?}"
                );
            }
        }
    }
}
