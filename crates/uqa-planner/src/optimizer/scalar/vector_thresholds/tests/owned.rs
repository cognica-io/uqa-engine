//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn admitted_merge_moves_live_child_owners_and_freezes_only_surviving_allocations() {
    let source = ScalarExpr::And(vec![
        call(
            "z",
            0.2,
            ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024))),
        ),
        column("ordinary"),
        call("z", 0.8, ScalarExpr::Literal(Value::Str("y".repeat(8192)))),
        call("a", 0.4, column("first_a")),
    ]);
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::And(items) = &*input else {
        unreachable!()
    };
    let buffer = items.as_ptr();
    let ScalarExpr::Func { args, .. } = &items[0] else {
        unreachable!()
    };
    let args_buffer = args.as_ptr();
    let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
        unreachable!()
    };
    let payload = text.as_ptr();
    let sibling = budget.reserve(37).unwrap();
    let before = budget.used();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            merge_owned_vector_thresholds(
                input,
                MergeControl {
                    scratch: control,
                    storage: control,
                },
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert!(measured.bytes_total < 16384, "{measured:?}");
    assert!(before - budget.used() > 8192);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes() + sibling.bytes(), budget.used());
    let ScalarExpr::And(items) = &*output else {
        panic!("conjunction")
    };
    assert_eq!(items.as_ptr(), buffer);
    assert_eq!(
        &items[..2],
        &[column("ordinary"), call("a", 0.4, column("first_a"))]
    );
    let ScalarExpr::Func { args, .. } = &items[2] else {
        panic!("first z call")
    };
    assert_eq!(args.as_ptr(), args_buffer);
    let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
        panic!("original payload")
    };
    assert_eq!(text.as_ptr(), payload);
    assert_eq!(args[2], ScalarExpr::Literal(Value::Float(0.8)));
    let before = budget.used();
    let mut frozen = None;
    let measured = allocation_counter::measure(|| {
        frozen = Some(output.finish(&control).unwrap());
    });
    let frozen = frozen.unwrap();
    assert_eq!(measured.count_total, 0);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(frozen.reserved_bytes() + sibling.bytes(), budget.used());
    assert_eq!(
        *frozen,
        merge_vector_thresholds(source, &ProductionControl::uncontrolled()).unwrap()
    );
    drop(frozen);
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn nested_admitted_merge_releases_rewritten_and_unvisited_owners_on_failure() {
    let source = ScalarExpr::Or(vec![
        ScalarExpr::And(vec![
            call("first", 0.2, column("first")),
            call("first", 0.8, column("discard")),
        ]),
        ScalarExpr::And(
            (0..512)
                .map(|index| call(&format!("f{index}"), 0.5, column("x")))
                .collect(),
        ),
        ScalarExpr::Not(Box::new(call("unvisited", 0.3, column("tail")))),
    ]);
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let sibling = budget.reserve(budget.available() - 4096).unwrap();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(merge_owned_vector_thresholds(
            input,
            MergeControl {
                scratch: control,
                storage: control,
            },
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(measured.bytes_total < 16384, "{measured:?}");
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(budget.used(), 0);
    let input = source.copy_for_rewrite(&control).unwrap();
    let output = merge_owned_vector_thresholds(
        input,
        MergeControl {
            scratch: control,
            storage: control,
        },
    )
    .unwrap();
    let expected = merge_vector_thresholds(source, &ProductionControl::uncontrolled()).unwrap();
    assert_eq!(*output, expected);
    assert_eq!(output.reserved_bytes(), budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn admitted_merge_cancellation_drops_the_original_payload_owners() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = ScalarExpr::And(vec![
            call("a", 0.4, ScalarExpr::Literal(Value::Str("x".repeat(8192)))),
            call("a", 0.7, column("discard")),
        ])
        .copy_for_rewrite(&control)
        .unwrap();
        let sibling = budget.reserve(37).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            merge_owned_vector_thresholds(
                input,
                MergeControl {
                    scratch: control,
                    storage: control
                }
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
