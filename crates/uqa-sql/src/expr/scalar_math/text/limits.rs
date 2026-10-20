//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn eval_scalar_function(name: &str, args: &[Value]) -> Result<Value, crate::SQLError> {
    crate::expr::eval_scalar_function(
        name,
        args,
        crate::catalog::collations::DEFAULT_COLLATION_OID as u32,
    )
}

// PostgreSQL 18.4, UTF8: oracle_compat.c lpad/rpad/repeat include VARHDRSZ in MaxAllocSize; padding uses the encoding's maximum character width.
#[test]
fn text_size_limits_reject_oversized_results_before_allocator_or_quota() {
    let cases = [
        ("lpad", vec![text("x"), Value::Int(268_435_455)]),
        ("rpad", vec![text("x"), Value::Int(536_870_911)]),
        ("repeat", vec![text("abcd"), Value::Int(268_435_455)]),
        ("repeat", vec![text("é"), Value::Int(536_870_910)]),
    ];
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let allocations = allocation_counter::measure(|| {
        for (name, args) in &cases {
            let error = eval_scalar_function(name, args).unwrap_err();
            assert_eq!(error.sqlstate(), Some("54000"));
            assert_eq!(error.to_string(), "requested length too large");
            let error = super::super::eval_math_functions_with_control(name, args, &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("54000"));
            assert_eq!(budget.used(), 0);
        }
        // The immediately preceding valid size reaches the caller's quota rather than the SQL length diagnostic, without allocating the result.
        for (name, args) in [
            ("lpad", vec![text("x"), Value::Int(268_435_454)]),
            ("repeat", vec![text("abcd"), Value::Int(268_435_454)]),
        ] {
            let error = super::super::eval_math_functions_with_control(name, &args, &control)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!(budget.used(), 0);
        }
    });
    assert!(allocations.bytes_max < 64 * 1024, "{allocations:?}");
}

#[test]
fn text_size_limits_preserve_empty_fill_null_negative_and_int4_semantics() {
    for name in ["lpad", "rpad", "repeat"] {
        for count in [i64::MIN, i64::MAX] {
            let error = eval_scalar_function(name, &[text("x"), Value::Int(count)]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
            assert_eq!(error.to_string(), "integer out of range");
        }
        assert_eq!(
            eval_scalar_function(name, &[text("é"), Value::Int(i64::from(i32::MIN))]).unwrap(),
            text("")
        );
        assert_eq!(
            eval_scalar_function(name, &[Value::Null, Value::Int(i64::from(i32::MAX))]).unwrap(),
            Value::Null
        );
    }
    for name in ["lpad", "rpad"] {
        assert_eq!(
            eval_scalar_function(
                name,
                &[text("é"), Value::Int(i64::from(i32::MAX)), text("")]
            )
            .unwrap(),
            text("é")
        );
        assert_eq!(
            eval_scalar_function(name, &[text("éab"), Value::Int(2)]).unwrap(),
            text("éa")
        );
        assert_eq!(
            eval_scalar_function(name, &[text("x"), Value::Int(536_870_911), Value::Null]).unwrap(),
            Value::Null
        );
    }
}
