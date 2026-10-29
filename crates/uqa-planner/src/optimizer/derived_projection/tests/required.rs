//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn derived_query(width: usize) -> QueryBlockPlan {
    let crate::UnifiedPlan::Query(query) = crate::UnifiedPlan::lower(
        uqa_sql::compile("SELECT first FROM (SELECT 1 AS first) q")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(mut outer) = query.root else {
        panic!("outer block");
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut outer.from else {
        panic!("derived source");
    };
    **body = super::demand::wide_query(width);
    let RelationalPlan::QueryBlock(inner) = &body.root else {
        panic!("inner block");
    };
    outer.projections = inner.projections.clone();
    for projection in &mut outer.projections {
        projection.expr = ScalarExpr::Column(projection.alias.take().unwrap().into());
    }
    *outer
}

#[test]
fn derived_required_names_reject_before_copying_wide_inputs() {
    let mut block = derived_query(1024);
    let before = serde_json::to_value(&block).unwrap();
    let parent = MemoryBudget::new(1024);
    let sibling = parent.reserve(512).unwrap();
    let budget = parent.child(128);
    let token = CancellationToken::new();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(prune(
            &mut block,
            OutputPruningContext {
                production: ProductionControl::new(&budget, &token, &token),
                ..OutputPruningContext::new(&NoAggregates)
            },
        ));
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&block).unwrap(), before);
}

#[test]
fn repeated_required_names_borrow_original_storage_and_ignore_diagnostic_positions() {
    let mut block = derived_query(1);
    block.projections.resize(4096, block.projections[0].clone());
    let pointers: Vec<_> = block
        .projections
        .iter_mut()
        .enumerate()
        .map(|(index, projection)| {
            let ScalarExpr::Column(name) = &mut projection.expr else {
                panic!("column");
            };
            *name = std::mem::take(name).with_position(Some(u32::try_from(index + 1).unwrap()));
            name.as_ptr()
        })
        .collect();
    let before = serde_json::to_value(&block).unwrap();
    let parent = MemoryBudget::new(1024);
    let sibling = parent.reserve(512).unwrap();
    let budget = parent.child(256);
    let token = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        prune(
            &mut block,
            OutputPruningContext {
                production: ProductionControl::new(&budget, &token, &token),
                ..OutputPruningContext::new(&NoAggregates)
            },
        )
        .unwrap();
    });
    assert!(allocation.bytes_total < 2048, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&block).unwrap(), before);
    for (index, (projection, pointer)) in block.projections.iter().zip(pointers).enumerate() {
        let ScalarExpr::Column(name) = &projection.expr else {
            panic!("column");
        };
        assert_eq!(name.as_ptr(), pointer);
        assert_eq!(name.position(), Some(u32::try_from(index + 1).unwrap()));
    }
}

struct ObservedCatalog<'a> {
    budget: &'a MemoryBudget,
    unwind: bool,
    calls: std::cell::Cell<usize>,
}

impl uqa_sql::semantics::volatility::VolatilityCatalog for ObservedCatalog<'_> {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }

    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        assert!(
            self.budget.used() > 0 && self.budget.used() <= 8,
            "only the positional demand buffer may remain during output inspection"
        );
        self.calls.set(self.calls.get() + 1);
        assert!(!self.unwind, "catalog observation unwound");
        uqa_sql::semantics::volatility::VolatilityCatalog::view_query(&Catalog, name)
    }
}

#[test]
fn required_name_nodes_release_before_output_inspection_and_its_errors_or_unwind() {
    for unwind in [false, true] {
        let crate::UnifiedPlan::Query(query) = crate::UnifiedPlan::lower(
            uqa_sql::compile(
                "SELECT first FROM (SELECT 1 AS first, (SELECT * FROM broken) AS unused) q",
            )
            .unwrap()
            .remove(0),
        ) else {
            panic!("query");
        };
        let RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("block");
        };
        let before = serde_json::to_value(&block).unwrap();
        let parent = MemoryBudget::new(4096);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(1024);
        let token = CancellationToken::new();
        let catalog = ObservedCatalog {
            budget: &budget,
            unwind,
            calls: std::cell::Cell::new(0),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prune(
                &mut block,
                OutputPruningContext {
                    production: ProductionControl::new(&budget, &token, &token),
                    volatility: Some(&catalog),
                    ..OutputPruningContext::new(&NoAggregates)
                },
            )
        }));
        if unwind {
            assert!(result.is_err());
        } else {
            assert_eq!(
                result.unwrap().unwrap_err().to_string(),
                "internal error: catalog lookup failed"
            );
        }
        assert_eq!(catalog.calls.get(), 1);
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        assert_eq!(serde_json::to_value(&block).unwrap(), before);
    }
}

#[test]
fn required_name_discovery_observes_both_cancellation_owners_before_allocation() {
    for cancel_original in [false, true] {
        let mut block = derived_query(1024);
        let before = serde_json::to_value(&block).unwrap();
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(prune(
                &mut block,
                OutputPruningContext {
                    production: ProductionControl::new(&budget, &original, &invoking),
                    ..OutputPruningContext::new(&NoAggregates)
                },
            ));
        });
        let error = result.unwrap().unwrap_err();
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.peak(), 0);
        assert_eq!(serde_json::to_value(&block).unwrap(), before);
    }
}
