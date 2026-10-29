//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

pub(super) fn wide_query(width: usize) -> QueryPlan {
    let crate::UnifiedPlan::Query(mut query) =
        crate::UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let template = block.projections.pop().unwrap();
    block.projections = (0..width)
        .map(|index| {
            let mut projection = template.clone();
            projection.alias = Some(format!("c{index:04}_{}", "x".repeat(50)));
            projection
        })
        .collect();
    *query
}

fn observe_wide_output(use_schema: bool) {
    let mut query = wide_query(1024);
    let names = uqa_sql::semantics::query_plan_output_columns(&query).unwrap();
    let schema = RowSchema::with_types(names, vec![Some(uqa_sql::ColumnType::Integer); 1024]);
    let before = serde_json::to_value(&query.root).unwrap();
    let calls = std::cell::Cell::new(0);
    let mut result = None;
    let allocations = allocation_counter::measure(|| {
        result = Some(prune_outputs(
            &mut query,
            &[],
            &|name| {
                let index = calls.get();
                assert_eq!(name, schema.columns()[index]);
                calls.set(index + 1);
                true
            },
            use_schema.then_some(&schema),
            OutputPruningContext::new(&NoAggregates),
        ));
    });
    result.unwrap().unwrap();
    assert_eq!(calls.get(), 1024);
    assert!(allocations.bytes_total < 8 * 1024, "{allocations:?}");
    assert_eq!(serde_json::to_value(&query.root).unwrap(), before);
}

#[test]
fn known_output_demand_borrows_wide_column_names() {
    observe_wide_output(true);
}

#[test]
fn inferred_output_demand_borrows_wide_projection_names() {
    observe_wide_output(false);
}

#[test]
fn output_demand_admits_scratch_before_growth_and_preserves_the_input_on_rejection() {
    for use_schema in [false, true] {
        let mut query = wide_query(1024);
        let before = serde_json::to_value(&query).unwrap();
        let schema = RowSchema::with_types(
            uqa_sql::semantics::query_plan_output_columns(&query).unwrap(),
            vec![Some(uqa_sql::ColumnType::Integer); 1024],
        );
        let parent = MemoryBudget::new(4096);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(128);
        let token = CancellationToken::new();
        let calls = std::cell::Cell::new(0);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(prune_outputs(
                &mut query,
                &[],
                &|_| {
                    calls.set(calls.get() + 1);
                    assert!(budget.used() > 0);
                    true
                },
                use_schema.then_some(&schema),
                OutputPruningContext {
                    production: ProductionControl::new(&budget, &token, &token),
                    ..OutputPruningContext::new(&NoAggregates)
                },
            ));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert!(calls.get() < 1024);
        if use_schema {
            assert_eq!(calls.get(), 0);
        }
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        assert_eq!(serde_json::to_value(&query).unwrap(), before);

        let retry = parent.child(2048);
        prune_outputs(
            &mut query,
            &[],
            &|_| true,
            use_schema.then_some(&schema),
            OutputPruningContext {
                production: ProductionControl::new(&retry, &token, &token),
                ..OutputPruningContext::new(&NoAggregates)
            },
        )
        .unwrap();
        assert_eq!(retry.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn output_demand_observes_original_names_with_live_scratch_and_alias_precedence() {
    for descriptor in 0..3 {
        let mut query = wide_query(1024);
        let names = uqa_sql::semantics::query_plan_output_columns(&query).unwrap();
        let schema = RowSchema::with_types(names, vec![Some(uqa_sql::ColumnType::Integer); 1024]);
        if descriptor == 1 {
            query.analyzed_output = Some(schema.clone());
        }
        let aliases = ["override".to_owned()];
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        let pointers: Vec<_> = (0..1024)
            .map(|index| {
                aliases.get(index).map_or_else(
                    || {
                        if descriptor == 0 {
                            block.projections[index].alias.as_ref().unwrap().as_ptr()
                        } else {
                            schema.columns()[index].as_ptr()
                        }
                    },
                    |name| name.as_ptr(),
                )
            })
            .collect();
        let parent = MemoryBudget::new(4096);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(2048);
        let token = CancellationToken::new();
        let index = std::cell::Cell::new(0);
        prune_outputs(
            &mut query,
            &aliases,
            &|name| {
                assert_eq!(name.as_ptr(), pointers[index.get()]);
                assert!(budget.used() > 0);
                assert_eq!(parent.used(), sibling.bytes() + budget.used());
                index.set(index.get() + 1);
                true
            },
            (descriptor == 2).then_some(&schema),
            OutputPruningContext {
                production: ProductionControl::new(&budget, &token, &token),
                ..OutputPruningContext::new(&NoAggregates)
            },
        )
        .unwrap();
        assert_eq!(index.get(), 1024);
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn output_demand_releases_partial_scratch_on_both_cancellations_and_unwind() {
    for use_schema in [false, true] {
        for action in 0..3 {
            let mut query = wide_query(32);
            let before = serde_json::to_value(&query).unwrap();
            let schema = RowSchema::with_types(
                uqa_sql::semantics::query_plan_output_columns(&query).unwrap(),
                vec![Some(uqa_sql::ColumnType::Integer); 32],
            );
            let parent = MemoryBudget::new(4096);
            let sibling = parent.reserve(512).unwrap();
            let budget = parent.child(1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let calls = std::cell::Cell::new(0);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prune_outputs(
                    &mut query,
                    &[],
                    &|_| {
                        calls.set(calls.get() + 1);
                        assert!(budget.used() > 0);
                        if calls.get() == 3 {
                            match action {
                                0 => original.cancel(),
                                1 => invoking.cancel(),
                                _ => panic!("demand callback unwound"),
                            }
                        }
                        true
                    },
                    use_schema.then_some(&schema),
                    OutputPruningContext {
                        production: ProductionControl::new(&budget, &original, &invoking),
                        ..OutputPruningContext::new(&NoAggregates)
                    },
                )
            }));
            if action < 2 {
                assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
            } else {
                assert!(result.is_err());
            }
            assert_eq!(calls.get(), 3);
            assert_eq!(budget.used(), 0);
            assert_eq!(parent.used(), sibling.bytes());
            assert_eq!(serde_json::to_value(&query).unwrap(), before);
        }
    }
}
