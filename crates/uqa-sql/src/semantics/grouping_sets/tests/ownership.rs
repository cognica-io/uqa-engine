//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn mutable_grouping_selection_retires_discarded_sets_and_analysis_storage() {
    let UnifiedPlan::Query(mut source) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        panic!("query block");
    };
    block.group_distinct = true;
    block.grouping_sets = ["same", "same", "different"]
        .map(|value| vec![ScalarExpr::Literal(Value::Str(value.repeat(2048)))])
        .into();
    source.analyzed_grouping = Some(AnalyzedGroupingSets {
        original_count: 3,
        retained: vec![0, 2],
    });
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut changed = None;
    let allocation = allocation_counter::measure(|| {
        changed = Some(copied.apply_grouping(&control).unwrap());
    });
    let changed = changed.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert_eq!(changed.reserved_bytes(), budget.used());
    assert!(changed.analyzed_grouping.is_none());
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        panic!("query block");
    };
    assert!(!block.group_distinct);
    assert_eq!(block.grouping_sets.len(), 2);
    for (set, value) in block.grouping_sets.iter().zip(["same", "different"]) {
        assert_eq!(set, &[ScalarExpr::Literal(Value::Str(value.repeat(2048)))]);
    }
    drop(changed);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_grouping_identity_keeps_only_serialized_output_until_release() {
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = input();
    for sql in [
        "SELECT id, t.id, n + 1, -42, coalesce(n,1), lower(NULL), NULL::text, '3'::integer, CASE WHEN n>0 THEN 1 ELSE 2 END FROM t",
        "SELECT sum(n) FILTER (WHERE n>0) OVER (PARTITION BY id ORDER BY n ROWS BETWEEN 1 PRECEDING AND 2 FOLLOWING) FROM t",
        "SELECT ROW(1,'a'), ARRAY[1,2], n BETWEEN 1 AND 2, n IN (1,2), n IS NULL, NOT n>0 FROM t",
    ] {
        let block = block(sql);
        for projection in &block.projections {
            let expression = crate::bind_type_introspection_with_resolver(projection.expr.clone(), &schema, &[], &Catalog);
            let normalized = normalize_expression(&Catalog, expression, &schema, &[]).unwrap();
            let expected = serde_json::to_vec(&normalized).unwrap();
            let mut output = None;
            let allocations = allocation_counter::measure(|| {
                output = Some(expression_identity_with_control(&Catalog, &projection.expr, &schema, &[], &control).unwrap());
            });
            let output = output.unwrap();
            assert_eq!(&*output, &expected, "{sql}");
            assert_eq!(budget.used(), output.reserved_bytes());
            assert_eq!(allocations.bytes_current as usize, budget.used());
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn controlled_distinct_grouping_keeps_first_normalized_set_and_only_output_capacity() {
    let original = block("SELECT n + 1 AS shifted FROM t GROUP BY DISTINCT GROUPING SETS ((shifted,n),(n,n+1),(n),(n),())");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = input();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(
            analyze_distinct_grouping_with_control(
                &Catalog,
                &original,
                &schema,
                None,
                &[],
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(output.retained, [0, 2, 4]);
    assert_eq!(output.original_count, 5);
    assert_eq!(budget.used(), output.reserved_bytes());
    assert_eq!(allocations.bytes_current as usize, budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn grouping_production_rejects_large_payload_and_observes_each_cancellation_owner() {
    let source = ScalarExpr::Literal(Value::Bytes(vec![7; 4 << 20]));
    for mode in 0..3 {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        match mode {
            1 => original.cancel(),
            2 => invoking.cancel(),
            _ => {}
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = expression_identity_with_control(
            &Catalog,
            &source,
            &RowSchema::default(),
            &[],
            &control,
        )
        .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
        let error = super::super::serialization::identity(&source, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
    }
}
