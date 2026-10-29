//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn query(sql: &str) -> QueryPlan {
    let crate::UnifiedPlan::Query(query) =
        crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    *query
}

fn wide_query(width: usize) -> QueryPlan {
    let mut query = query("WITH q AS (SELECT 1 AS first) SELECT first FROM q");
    let RelationalPlan::QueryBlock(inner) = &mut query.ctes[0].body.query_mut().unwrap().root
    else {
        panic!("CTE block");
    };
    inner
        .projections
        .resize(width, inner.projections[0].clone());
    for (index, projection) in inner.projections.iter_mut().enumerate() {
        projection.alias = Some(format!("column_{index:049}"));
    }
    let mut projections = inner.projections.clone();
    for projection in &mut projections {
        projection.expr = ScalarExpr::Column(projection.alias.take().unwrap().into());
    }
    let RelationalPlan::QueryBlock(outer) = &mut query.root else {
        panic!("outer block");
    };
    outer.projections = projections;
    query
}

#[test]
fn folded_cte_workspace_rejects_before_copying_wide_inputs() {
    let mut query = wide_query(1024);
    let before = serde_json::to_value(&query).unwrap();
    let parent = MemoryBudget::new(1024);
    let sibling = parent.reserve(512).unwrap();
    let budget = parent.child(128);
    let token = CancellationToken::new();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(prune(
            &mut query,
            &Catalog,
            &Catalog,
            None,
            uqa_core::memory::ProductionControl::new(&budget, &token, &token),
        ));
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&query).unwrap(), before);
}

#[test]
fn folded_cte_partial_workspace_failure_releases_ancestor_allowance() {
    let mut query = wide_query(64);
    let before = serde_json::to_value(&query).unwrap();
    let token = CancellationToken::new();
    let mut partial_failure = false;
    let mut completed = false;
    for limit in [128, 2048, 4096, 8192, 16384] {
        let parent = MemoryBudget::new(32768);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(limit);
        match prune(
            &mut query,
            &Catalog,
            &Catalog,
            None,
            ProductionControl::new(&budget, &token, &token),
        ) {
            Ok(()) => completed = true,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                partial_failure |= budget.peak() > 2048;
            }
        }
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        assert_eq!(serde_json::to_value(&query).unwrap(), before);
    }
    assert!(partial_failure);
    assert!(completed);
}

#[test]
fn folded_cte_workspace_observes_both_cancellations_before_allocation() {
    for cancel_original in [false, true] {
        let mut query = wide_query(1024);
        let before = serde_json::to_value(&query).unwrap();
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
                &mut query,
                &Catalog,
                &Catalog,
                None,
                uqa_core::memory::ProductionControl::new(&budget, &original, &invoking),
            ));
        });
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(budget.peak(), 0);
        assert_eq!(serde_json::to_value(&query).unwrap(), before);
    }
}

#[test]
fn folded_cte_names_and_repeated_consumers_borrow_original_storage() {
    let mut query = wide_query(1);
    query.ctes[0].name = "x\"".repeat(32 * 1024);
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let Some(SourcePlan::Table { name, .. }) = &mut block.from else {
        panic!("CTE reference");
    };
    *name = format!("\"{}\"", query.ctes[0].name.replace('"', "\"\""));
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
    let before = serde_json::to_value(&query).unwrap();
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let budget = parent.child(4096);
    let token = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        prune(
            &mut query,
            &Catalog,
            &Catalog,
            None,
            ProductionControl::new(&budget, &token, &token),
        )
        .unwrap();
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert!(budget.peak() <= 4096);
    assert_eq!(budget.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&query).unwrap(), before);
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block");
    };
    for (index, (projection, pointer)) in block.projections.iter().zip(pointers).enumerate() {
        let ScalarExpr::Column(name) = &projection.expr else {
            panic!("column");
        };
        assert_eq!(name.as_ptr(), pointer);
        assert_eq!(name.position(), Some(u32::try_from(index + 1).unwrap()));
    }
}

#[test]
fn folded_cte_selection_retains_lexical_order_and_releases_schedule_storage() {
    let query =
        query("WITH z AS (SELECT 1), a AS (SELECT 2), m AS (SELECT 3) SELECT * FROM z, m, a");
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let indices = folded_ctes(
        &query,
        &Catalog,
        ProductionControl::new(&budget, &token, &token),
    )
    .unwrap();
    assert_eq!(&*indices, [1, 2, 0]);
    assert_eq!(budget.used(), indices.len() * std::mem::size_of::<usize>());
    drop(indices);
    assert_eq!(budget.used(), 0);
}

#[test]
fn folded_cte_demand_keeps_quoted_names_duplicate_aliases_and_analyzed_authority() {
    for (sql, expected) in [
        ("WITH \"q\"\"r\" AS (SELECT 1 AS kept, 2 AS unused) SELECT kept FROM \"q\"\"r\"", [false, true]),
        ("WITH q AS (SELECT 1 AS same, 2 AS same) SELECT x FROM q AS r(x,y)", [false, false]),
        ("WITH q AS (SELECT 1 AS kept, 2 AS unused) SELECT kept FROM public.q", [false, false]),
        ("WITH q AS (SELECT 1 AS kept, 2 AS unused) SELECT kept FROM (WITH q AS (SELECT 3 AS kept) SELECT kept FROM q) r", [false, false]),
        ("WITH q AS (SELECT 1 AS kept, 2 AS unused) SELECT kept FROM (SELECT kept FROM q) r", [false, true]),
        ("WITH q AS (SELECT 1 AS kept, 2 AS unused) SELECT q FROM q", [false, false]),
        ("WITH q AS (SELECT 1 AS q, 2 AS unused) SELECT q FROM q", [false, true]),
    ] {
        let mut query = query(sql);
        let budget = MemoryBudget::new(16384);
        let token = CancellationToken::new();
        prune(&mut query, &Catalog, &Catalog, None, ProductionControl::new(&budget, &token, &token)).unwrap();
        let RelationalPlan::QueryBlock(block) = &query.ctes[0].body.query().unwrap().root else { panic!("CTE block"); };
        assert_eq!(block.projections.iter().map(|p| matches!(p.expr, ScalarExpr::TypedLiteral { value: uqa_core::Value::Null, .. })).collect::<Vec<_>>(), expected, "{sql}");
        assert_eq!(budget.used(), 0);
    }
    let mut query = query("WITH q AS (SELECT 1 AS kept, 2 AS unused) SELECT kept FROM q");
    query.ctes[0].body.query_mut().unwrap().analyzed_output = Some(uqa_sql::RowSchema::new(vec![
        "different".into(),
        "kept".into(),
    ]));
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    prune(
        &mut query,
        &Catalog,
        &Catalog,
        None,
        ProductionControl::new(&budget, &token, &token),
    )
    .unwrap();
    let RelationalPlan::QueryBlock(block) = &query.ctes[0].body.query().unwrap().root else {
        panic!("CTE block");
    };
    assert!(matches!(
        block.projections[0].expr,
        ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Null,
            ..
        }
    ));
    assert!(!matches!(
        block.projections[1].expr,
        ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Null,
            ..
        }
    ));
    assert_eq!(budget.used(), 0);
}

struct ObservedCatalog<'a> {
    budget: &'a MemoryBudget,
    token: &'a CancellationToken,
    action: usize,
    calls: std::cell::Cell<usize>,
}

impl VolatilityCatalog for ObservedCatalog<'_> {
    fn host_function_volatility(&self, _: &str) -> Option<uqa_sql::ast::FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
    ) -> Option<Vec<uqa_sql::ast::FunctionVolatility>> {
        None
    }
    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        assert_eq!(name, "observed");
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() == 1 {
            // Scheduling observes volatility before discovering output demand.
            return Ok(None);
        }
        assert!(
            self.budget.used() > 0 && self.budget.used() <= 40,
            "only the selected indices and positional demand may remain: {}",
            self.budget.used()
        );
        match self.action {
            0 => Err(SQLError::Internal("output inspection failed".into())),
            1 => panic!("output inspection unwound"),
            _ => {
                self.token.cancel();
                Ok(None)
            }
        }
    }
}

#[test]
fn folded_cte_name_owners_retire_before_output_errors_cancellation_and_unwind() {
    for action in 0..4 {
        let mut query = query(
            "WITH q AS (SELECT 1 AS kept, (SELECT * FROM observed) AS unused) SELECT kept FROM q",
        );
        let before = serde_json::to_value(&query).unwrap();
        let parent = MemoryBudget::new(16384);
        let sibling = parent.reserve(512).unwrap();
        let budget = parent.child(8192);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let catalog = ObservedCatalog {
            budget: &budget,
            token: if action == 2 { &original } else { &invoking },
            action,
            calls: std::cell::Cell::new(0),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prune(
                &mut query,
                &Catalog,
                &catalog,
                None,
                ProductionControl::new(&budget, &original, &invoking),
            )
        }));
        match action {
            0 => assert_eq!(
                result.unwrap().unwrap_err().to_string(),
                "internal error: output inspection failed"
            ),
            1 => assert!(result.is_err()),
            _ => assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014")),
        }
        assert_eq!(catalog.calls.get(), 2);
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        assert_eq!(serde_json::to_value(&query).unwrap(), before);
    }
}
