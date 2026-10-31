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

fn plan(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *plan
}
fn block(plan: &QueryPlan) -> &QueryBlockPlan {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        panic!("block")
    };
    block
}

#[test]
fn placement_and_residual_producers_reject_before_copying_and_retain_exact_owners() {
    let fixture = Fixture::new();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for placed in [false, true] {
        let mut query = plan("SELECT v.x FROM(VALUES(1)) v(x) WHERE v.x>0");
        let RelationalPlan::QueryBlock(stmt) = &mut query.root else {
            panic!("block")
        };
        let payload = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
        stmt.r#where = Some(if placed {
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(ScalarExpr::qualified_column("v", "x")),
                rhs: Box::new(payload),
            }
        } else {
            payload
        });
        let from = stmt.from.as_ref().unwrap();
        for failure in 0..3 {
            let budget = MemoryBudget::new(32 << 10);
            let control = ProductionControl::new(&budget, &original, &invoking);
            if failure == 1 {
                original.cancel();
            }
            if failure == 2 {
                invoking.cancel();
            }
            let allocation = allocation_counter::measure(|| {
                let error = if placed {
                    qualifier_filters_for_stmt_with_control(
                        fixture.context(),
                        stmt,
                        from,
                        fixture.scope(),
                        &control,
                    )
                    .err()
                    .unwrap()
                } else {
                    final_filter_after_qualifier_pushdown_with_control(
                        fixture.context(),
                        stmt,
                        from,
                        None,
                        fixture.scope(),
                        &control,
                    )
                    .err()
                    .unwrap()
                };
                assert_eq!(
                    error.sqlstate(),
                    Some(if failure == 0 { "53200" } else { "57014" })
                );
            });
            assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
            assert_eq!(budget.used(), 0);
            original.reset();
            invoking.reset();
        }
        let budget = MemoryBudget::new(32 << 20);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut filters = None;
        let mut residual = None;
        let allocation = allocation_counter::measure(|| {
            if placed {
                filters = qualifier_filters_for_stmt_with_control(
                    fixture.context(),
                    stmt,
                    from,
                    fixture.scope(),
                    &control,
                )
                .unwrap();
            } else {
                residual = final_filter_after_qualifier_pushdown_with_control(
                    fixture.context(),
                    stmt,
                    from,
                    None,
                    fixture.scope(),
                    &control,
                )
                .unwrap();
            }
        });
        assert_eq!(allocation.bytes_current, budget.used() as i64);
        if let Some(filters) = &filters {
            assert_eq!(&filters["v"], &[stmt.r#where.clone().unwrap()]);
        }
        if let Some(residual) = &residual {
            assert_eq!(&**residual, stmt.r#where.as_ref().unwrap());
        }
        drop(filters);
        drop(residual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn necessary_disjunction_and_single_reference_cte_filters_keep_order_and_cleanup() {
    let fixture = Fixture::new();
    let query = plan("SELECT a.x,b.y FROM(VALUES(1)) a(x),(VALUES(2)) b(y) WHERE(a.x=1 AND b.y=2)OR(a.x=3 AND b.y=4)");
    let stmt = block(&query);
    let from = stmt.from.as_ref().unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut filters = None;
    let allocation = allocation_counter::measure(|| {
        filters = qualifier_filters_for_stmt_with_control(
            fixture.context(),
            stmt,
            from,
            fixture.scope(),
            &control,
        )
        .unwrap();
    });
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    let filters = filters.unwrap();
    for (name, sql) in [
        ("a", "SELECT a.x=1 OR a.x=3"),
        ("b", "SELECT b.y=2 OR b.y=4"),
    ] {
        let expected = plan(sql);
        assert_eq!(
            &filters[name],
            &[block(&expected).projections[0].expr.clone()]
        );
    }
    let residual = final_filter_after_qualifier_pushdown_with_control(
        fixture.context(),
        stmt,
        from,
        Some(&filters),
        fixture.scope(),
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(&*residual, stmt.r#where.as_ref().unwrap());
    drop(residual);
    drop(filters);
    assert_eq!(budget.used(), 0);
    for (sql, count) in [
        (
            "WITH c AS(SELECT 1 AS x) SELECT * FROM c AS v WHERE v.x>0 AND v.x<3",
            1,
        ),
        (
            "WITH c AS(SELECT 1 AS x) SELECT * FROM c AS v,c AS w WHERE v.x>0",
            0,
        ),
    ] {
        let query = plan(sql);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(
                cte_output_filters_with_control(
                    fixture.context(),
                    &query,
                    fixture.scope(),
                    &control,
                )
                .unwrap(),
            );
        });
        assert_eq!(allocation.bytes_current, budget.used() as i64);
        assert_eq!(output.as_ref().unwrap().len(), count);
        if count == 1 {
            let (name, predicate) = output.as_ref().unwrap().get("c").unwrap();
            assert_eq!(name.as_str(), "v");
            assert_eq!(&**predicate, block(&query).r#where.as_ref().unwrap());
        }
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn equality_propagation_admits_selected_constants_without_mutating_source_filters() {
    let query = plan("SELECT * FROM(VALUES(1)) a(x) JOIN(VALUES(2)) b(y) ON a.x=b.y");
    let SourcePlan::Join {
        left, right, on, ..
    } = block(&query).from.as_ref().unwrap()
    else {
        panic!("join")
    };
    let source = qualified_literal_equality("a", "x", &"x".repeat(4 << 20));
    let filters = QualifierFilters::from([("a".into(), vec![source])]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let small = MemoryBudget::new(32 << 10);
    let control = ProductionControl::new(&small, &original, &invoking);
    for failure in 0..3 {
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                crate::source_filters::propagated_join_filters_with_control(
                    &filters,
                    left,
                    right,
                    on.as_ref(),
                    &control
                )
                .err()
                .unwrap()
                .sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
        });
        assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
        assert_eq!(small.used(), 0);
        original.reset();
        invoking.reset();
    }
    let budget = MemoryBudget::new(32 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = crate::source_filters::propagated_join_filters_with_control(
            &filters,
            left,
            right,
            on.as_ref(),
            &control,
        )
        .unwrap();
    });
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    let output = output.unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(&output["a"], &filters["a"]);
    assert_eq!(
        &output["b"],
        &[qualified_literal_equality("b", "y", &"x".repeat(4 << 20))]
    );
    assert_eq!(filters.len(), 1);
    drop(output);
    assert_eq!(budget.used(), 0);
}
#[test]
fn derived_filter_specialization_admits_query_storage_and_preserves_input_owners() {
    let fixture = Fixture::new();
    let input = plan(&format!(
        "SELECT x AS k,'{}' AS payload FROM (VALUES(1),(2)) v(x)",
        "x".repeat(4 << 20)
    ));
    let predicate = qualified_literal_equality("q", "k", "2");
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for recursive in [false, true] {
        let invoke = |control: &ProductionControl<'_>| {
            if recursive {
                push_output_filter_into_query_plan_with_control(
                    fixture.context(),
                    &input,
                    "q",
                    &predicate,
                    None,
                    control,
                )
            } else {
                push_derived_output_filter_with_control(
                    fixture.context(),
                    &input,
                    "q",
                    &predicate,
                    None,
                    control,
                )
            }
        };
        let tiny = MemoryBudget::new(32 << 10);
        let control = ProductionControl::new(&tiny, &original, &invoking);
        for failure in 0..3 {
            if failure == 1 {
                original.cancel();
            }
            if failure == 2 {
                invoking.cancel();
            }
            let allocation = allocation_counter::measure(|| {
                assert_eq!(
                    invoke(&control).unwrap_err().sqlstate(),
                    Some(if failure == 0 { "53200" } else { "57014" })
                );
            });
            assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
            assert_eq!(tiny.used(), 0);
            original.reset();
            invoking.reset();
        }
        let budget = MemoryBudget::new(32 << 20);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = invoke(&control).unwrap();
        });
        let output = output.unwrap();
        assert_eq!(allocation.bytes_current, budget.used() as i64);
        assert_eq!(budget.used(), output.reserved_bytes());
        assert_eq!(
            serde_json::to_value(&block(&output).projections).unwrap(),
            serde_json::to_value(&block(&input).projections).unwrap(),
        );
        assert_eq!(block(&input).r#where, None);
        assert!(block(&output).r#where.is_some());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
