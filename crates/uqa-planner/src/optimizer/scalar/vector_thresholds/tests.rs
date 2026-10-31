//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::ast::FunctionOrderSyntax;

mod owned;

fn call(field: &str, threshold: f64, payload: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: FunctionOrderSyntax::Ordinary,
        name: "knn_match".into(),
        binding: None,
        args: vec![
            ScalarExpr::Literal(Value::Str(field.into())),
            payload,
            ScalarExpr::Literal(Value::Float(threshold)),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn column(name: &str) -> ScalarExpr {
    ScalarExpr::Column(name.into())
}

#[test]
fn merge_moves_first_payload_and_orders_fields_after_other_predicates() {
    let first = call(
        "z",
        0.25,
        ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024))),
    );
    let ScalarExpr::Func { args, .. } = &first else {
        unreachable!()
    };
    let buffer = args.as_ptr();
    let ScalarExpr::Literal(Value::Str(payload)) = &args[1] else {
        unreachable!()
    };
    let payload = payload.as_ptr();
    let items = vec![
        first,
        column("before"),
        call("a", 0.4, column("first_a")),
        call("z", 0.9, column("discard_z")),
        column("after"),
        call("a", 0.2, column("discard_a")),
    ];
    let container = items.as_ptr();
    let memory = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sibling = memory.reserve(123).unwrap();
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(merge_vector_thresholds(ScalarExpr::And(items), &control).unwrap());
    });
    assert!(allocated.bytes_total < 16 * 1024, "{allocated:?}");
    assert_eq!(memory.used(), sibling.bytes());
    let ScalarExpr::And(items) = output.unwrap() else {
        panic!("conjunction")
    };
    assert_eq!(items.as_ptr(), container);
    assert_eq!(
        &items[..3],
        &[
            column("before"),
            column("after"),
            call("a", 0.4, column("first_a"))
        ]
    );
    let ScalarExpr::Func { args, .. } = &items[3] else {
        panic!("retained first call")
    };
    assert_eq!(args.as_ptr(), buffer);
    let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
        panic!("payload")
    };
    assert_eq!(text.as_ptr(), payload);
    assert_eq!(args[2], ScalarExpr::Literal(Value::Float(0.9)));
    drop((items, sibling));
    assert_eq!(memory.used(), 0);
}

#[test]
fn merge_preserves_first_call_metadata_and_strict_float_comparison() {
    let nan = f64::from_bits(0x7ff8_0000_0000_0042);
    for (first, second, expected) in [
        (nan, 0.9, nan),
        (0.2, nan, 0.2),
        (-0.0, 0.0, -0.0),
        (0.0, -0.0, 0.0),
        (0.4, 0.4, 0.4),
        (0.4, f64::INFINITY, f64::INFINITY),
    ] {
        let mut original = call("f", first, column("first"));
        let ScalarExpr::Func {
            name,
            distinct,
            filter,
            ..
        } = &mut original
        else {
            unreachable!()
        };
        *name = "KNN_MATCH".into();
        *distinct = true;
        *filter = Some(Box::new(column("filter")));
        let original_name = name.as_ptr();
        let original_filter = std::ptr::from_ref(filter.as_deref().unwrap());
        let output = merge_vector_thresholds(
            ScalarExpr::And(vec![original, call("f", second, column("second"))]),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        let ScalarExpr::Func {
            name,
            args,
            distinct,
            filter,
            ..
        } = output
        else {
            panic!("single call")
        };
        assert_eq!(name.as_ptr(), original_name);
        assert!(distinct);
        assert_eq!(
            std::ptr::from_ref(filter.as_deref().unwrap()),
            original_filter
        );
        assert_eq!(args[1], column("first"));
        let ScalarExpr::Literal(Value::Float(actual)) = args[2] else {
            panic!("threshold")
        };
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[test]
fn merge_admits_scratch_before_copying_payloads_and_releases_partial_work() {
    for limit in [0, 128, 1024, 8192] {
        let input = ScalarExpr::And(
            (0..512)
                .map(|index| call(&format!("field_{index:04}"), 0.5, column("x")))
                .collect(),
        );
        let memory = MemoryBudget::new(limit + 37);
        let token = CancellationToken::new();
        let sibling = memory.reserve(37).unwrap();
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(merge_vector_thresholds(
                input,
                &ProductionControl::new(&memory, &token, &token),
            ));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(
            allocated.bytes_total < 32 * 1024,
            "limit={limit}: {allocated:?}"
        );
        assert_eq!(memory.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn merge_admits_placement_storage_after_the_field_index() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sibling = memory.reserve(37).unwrap();
    let single = ScalarExpr::And(vec![call("field", 0.5, column("x"))]);
    assert_eq!(
        merge_vector_thresholds(single, &control).unwrap(),
        call("field", 0.5, column("x"))
    );
    let input = ScalarExpr::And((0..512).map(|_| call("field", 0.5, column("x"))).collect());
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(merge_vector_thresholds(input, &control));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(allocated.bytes_total < 4096, "{allocated:?}");
    assert_eq!(memory.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn merge_checks_both_cancellation_scopes_and_can_recover() {
    for cancel_original in [true, false] {
        let memory = MemoryBudget::new(16 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let input = ScalarExpr::And(vec![
            call("a", 0.5, column("x")),
            call("a", 0.7, column("y")),
        ]);
        assert_eq!(
            merge_vector_thresholds(
                input.clone(),
                &ProductionControl::new(&memory, &original, &invoking)
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        let fresh = CancellationToken::new();
        let output =
            merge_vector_thresholds(input, &ProductionControl::new(&memory, &fresh, &fresh))
                .unwrap();
        assert_eq!(output, call("a", 0.7, column("x")));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn merge_reuses_nonmatching_containers_without_allocating() {
    let items = vec![
        ScalarExpr::Not(Box::new(column("a"))),
        ScalarExpr::And(vec![column("b"), column("c")]),
    ];
    let input = ScalarExpr::Or(items);
    let expected = input.clone();
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(merge_vector_thresholds(input, &ProductionControl::uncontrolled()).unwrap());
    });
    assert_eq!(allocated.bytes_total, 0, "{allocated:?}");
    assert_eq!(output.unwrap(), expected);
}

#[test]
fn merge_permutation_preserves_each_first_call_for_interleaved_fields() {
    for seed in 0..32 {
        let items = (0..97)
            .map(|index| {
                if (index + seed) % 5 == 0 {
                    column(&format!("other_{index}"))
                } else {
                    call(
                        &format!("field_{:02}", (index * 17 + seed) % 19),
                        f64::from(index),
                        column(&format!("payload_{index}")),
                    )
                }
            })
            .collect::<Vec<_>>();
        let mut expected = items
            .iter()
            .filter(|item| matches!(item, ScalarExpr::Column(_)))
            .cloned()
            .collect::<Vec<_>>();
        for field in 0..19 {
            let field = format!("field_{field:02}");
            let matches = items
                .iter()
                .filter(|item| threshold_call(item).is_some_and(|(key, _)| key == field))
                .collect::<Vec<_>>();
            if let Some(first) = matches.first() {
                let mut selected = (*first).clone();
                let max = matches
                    .iter()
                    .map(|item| threshold_call(item).unwrap().1)
                    .fold(f64::NEG_INFINITY, f64::max);
                let ScalarExpr::Func { args, .. } = &mut selected else {
                    unreachable!()
                };
                args[2] = ScalarExpr::Literal(Value::Float(max));
                expected.push(selected);
            }
        }
        assert_eq!(
            merge_vector_thresholds(ScalarExpr::And(items), &ProductionControl::uncontrolled())
                .unwrap(),
            ScalarExpr::And(expected),
            "seed={seed}"
        );
    }
}
