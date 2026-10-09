//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{eval_scalar, ProjectedPredicate, RowSchema, ScalarEvalContext, ScalarExpr};
use std::cell::Cell;
use uqa_sql::{ColumnType, ResultRow};

struct CountedFields<'a> {
    values: &'a [Value],
    reads: Cell<usize>,
}

impl FieldValues for CountedFields<'_> {
    fn field(&self, index: usize) -> &Value {
        self.reads.set(self.reads.get() + 1);
        self.values.get(index).unwrap_or(&NULL_VALUE)
    }
}

fn expression(source: &str) -> ScalarExpr {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("scalar SELECT");
    };
    uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

fn schema(names: &[&str]) -> RowSchema {
    RowSchema::with_types(
        names.iter().map(|name| (*name).to_owned()).collect(),
        vec![Some(ColumnType::BigInteger); names.len()],
    )
}

#[test]
fn parsed_analytical_ranges_read_each_needed_field_once() {
    let schema = schema(&["ship_day", "discount", "quantity"]);
    // The compiler expands BETWEEN before binding, and Planner flattens AND.
    // Cover both real parsed shapes instead of constructing legacy Between IR.
    for source in [
        "ship_day BETWEEN 365 AND 2190 AND discount BETWEEN 2 AND 8 AND quantity < 40",
        "ship_day >= 365 AND ship_day <= 2190 AND discount >= 2 AND discount <= 8 AND quantity < 40",
    ] {
        let predicate = ProjectedPredicate::compile_with_schema(&expression(source), &schema, &[])
            .unwrap().unwrap();
        for id in 0..20_000 {
            let sample = id ^ 20_260_802;
            let (day, discount, quantity) = (sample % 2_500, sample % 11, 1 + sample % 50);
            let day_matches = (365..=2190).contains(&day);
            let discount_matches = (2..=8).contains(&discount);
            let values = [Value::Int(day), Value::Int(discount), Value::Int(quantity)];
            let fields = CountedFields { values: &values, reads: Cell::new(0) };
            assert_eq!(keep_fields(&predicate.expression, &fields).unwrap(),
                day_matches && discount_matches && quantity < 40);
            let expected_reads = 1 + usize::from(day_matches)
                + usize::from(day_matches && discount_matches);
            assert_eq!(fields.reads.get(), expected_reads, "row {id}: {source}");
            assert_eq!(predicate.keep_indexed(&values, &[0, 1, 2]).unwrap(),
                day_matches && discount_matches && quantity < 40);
        }
    }
}

#[test]
fn projected_ranges_preserve_fallback_null_error_and_bound_order() {
    let schema = schema(&["x", "y"]);
    for source in [
        "x BETWEEN 1 AND 3",
        "1 <= x AND 3 >= x",
        "x BETWEEN 3 AND 1",
        "x BETWEEN 1 AND 3 AND 10 / y > 0",
        "x >= 1 AND y < 3 AND x <= 3",
        "x > 1 AND x < 3",
        "x <= 3 AND x >= 1",
        "x >= 1 OR x <= 3",
    ] {
        let scalar = expression(source);
        let predicate = ProjectedPredicate::compile_with_schema(&scalar, &schema, &[])
            .unwrap()
            .unwrap();
        for x in [
            Value::Null,
            Value::Int(0),
            Value::Int(2),
            Value::Int(4),
            Value::Float(2.5),
            Value::Float(f64::NAN),
            Value::Str("invalid".into()),
        ] {
            for y in [Value::Int(0), Value::Int(2), Value::Null] {
                let row = ResultRow::from([("x".into(), x.clone()), ("y".into(), y.clone())]);
                let expected = eval_scalar(&scalar, &ScalarEvalContext::new(Some(&row), &[]))
                    .map(|value| truthy(&value));
                let actual = predicate.keep(&[&x, &y]);
                match (expected, actual) {
                    (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{source}: {row:?}"),
                    (Err(expected), Err(actual)) => assert_eq!(
                        actual.to_string(),
                        expected.to_string(),
                        "{source}: {row:?}"
                    ),
                    (expected, actual) => panic!("{source}: {row:?}: {actual:?} != {expected:?}"),
                }
            }
        }
    }
}

#[test]
fn range_fusion_does_not_move_intervening_or_strict_comparisons() {
    let schema = schema(&["x", "y"]);
    for (source, reads) in [
        ("1 <= x AND 3 >= x", 2),
        ("x >= 1 AND y <= 3", 2),
        ("x >= 1 AND y >= 0 AND x <= 3", 3),
        ("x > 1 AND x < 3", 2),
        ("x <= 3 AND x >= 1", 2),
    ] {
        let predicate = ProjectedPredicate::compile_with_schema(&expression(source), &schema, &[])
            .unwrap()
            .unwrap();
        let fields = CountedFields {
            values: &[Value::Int(2), Value::Int(2)],
            reads: Cell::new(0),
        };
        assert!(keep_fields(&predicate.expression, &fields).unwrap());
        assert_eq!(fields.reads.get(), reads, "{source}");
    }
}
