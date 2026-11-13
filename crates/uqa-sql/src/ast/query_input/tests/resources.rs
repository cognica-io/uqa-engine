//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_query_input_retires_nested_native_allocations_on_denial_and_cancellation() {
    for index in [0, 1, 7, 11, 12] {
        let encoded = query(QUERIES[index]).to_string();
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(Expr::from_stored_query_json(&encoded, &memory, &token, &token).unwrap());
        });
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            memory.used()
        );
        assert!(usize::try_from(allocations.bytes_max).unwrap() <= memory.peak());
        let peak = memory.peak();
        drop(output);
        assert_eq!(memory.used(), 0);
        for limit in (0..peak).step_by((peak / 16).max(1)).chain([peak - 1]) {
            let memory = MemoryBudget::new(limit);
            let error =
                Expr::from_stored_query_json(&encoded, &memory, &token, &token).unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("53200"),
                "fixture {index}, limit {limit}/{peak}"
            );
            drop(error);
            assert_eq!(memory.used(), 0);
            assert!(memory.peak() <= limit);
        }
    }
    let mut expression = query(QUERIES[11]);
    let projection = expression["ScalarSubquery"]["projections"][0].clone();
    expression["ScalarSubquery"]["projections"] = vec![projection; 2048].into();
    let encoded = expression.to_string();
    let baseline = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let result = Expr::from_stored_query_json(&encoded, &baseline, &token, &token).unwrap();
    let retained = baseline.used();
    assert!(retained > 65536);
    drop(result);
    assert_eq!(baseline.used(), 0);
    for minimum in [retained / 4, retained / 2] {
        for original_cancelled in [true, false] {
            let memory = MemoryBudget::new(64 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let cancel = if original_cancelled {
                &original
            } else {
                &invoking
            };
            let result = crate::ast::stored_input::test_control::cancel_after_allocation(
                &memory,
                cancel,
                minimum,
                || Expr::from_stored_query_json(&encoded, &memory, &original, &invoking),
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
            assert!(memory.peak() >= minimum);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn stored_query_input_retains_only_surviving_merge_binding_payloads() {
    let mut expression = query(QUERIES[11]);
    let small = json!({"object_id":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"attribute_number":3,"domain_dependencies":[40000]});
    expression["ScalarSubquery"]["with"][0]["merge"]["target_column_bindings"] =
        json!({"kept":small.clone()});
    let baseline = expression.to_string();
    let mut large = small.clone();
    large["domain_dependencies"] = (40000..42000).collect::<Vec<u32>>().into();
    let duplicate = format!("{{\"kept\":{large},\"kept\":{small}}}");
    let expanded = baseline.replace(&json!({"kept":small}).to_string(), &duplicate);
    assert_ne!(baseline, expanded);
    let retained = |text: &str| {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let result = Expr::from_stored_query_json(text, &memory, &token, &token).unwrap();
        let used = memory.used();
        drop(result);
        assert_eq!(memory.used(), 0);
        used
    };
    assert_eq!(retained(&baseline), retained(&expanded));
    assert!(compare(&expanded));
}
