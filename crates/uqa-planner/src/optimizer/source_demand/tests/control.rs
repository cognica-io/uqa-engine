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

fn wide_query(width: usize) -> UnifiedPlan {
    let mut query = query("SELECT q.* FROM (SELECT 1 AS value) q");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        panic!("source");
    };
    let RelationalPlan::QueryBlock(inner) = &mut body.root else {
        panic!("inner block");
    };
    inner
        .projections
        .resize(width, inner.projections[0].clone());
    let names: Vec<_> = (0..width)
        .map(|index| format!("column_{index:049}"))
        .collect();
    for (projection, name) in inner.projections.iter_mut().zip(&names) {
        projection.alias = Some(name.clone());
    }
    query.analyzed_source = Some(
        Arc::new(AnalyzedSource {
            schema: RowSchema::with_qualified_types(
                "q",
                names,
                vec![Some(ColumnType::Integer); width],
            ),
            children: None,
        })
        .into(),
    );
    UnifiedPlan::Query(query)
}

#[test]
fn positional_wildcard_demand_rejects_before_copying_wide_descriptors() {
    let mut plan = wide_query(1024);
    let before = serde_json::to_value(&plan).unwrap();
    let parent = MemoryBudget::new(1024);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(128);
    let token = CancellationToken::new();
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = ProductionControl::new(&memory, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(prune(&mut plan, &config, &|_: &str| false));
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
    assert_eq!(serde_json::to_value(&plan).unwrap(), before);
}

#[test]
fn positional_demand_observes_both_cancellations_before_workspace_allocation() {
    for cancel_original in [false, true] {
        let mut plan = wide_query(1024);
        let before = serde_json::to_value(&plan).unwrap();
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let mut config =
            OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        config.production = ProductionControl::new(&memory, &original, &invoking);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(prune(&mut plan, &config, &|_: &str| false));
        });
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.peak(), 0);
        assert_eq!(serde_json::to_value(&plan).unwrap(), before);
    }
}

#[test]
fn positional_cte_names_reject_before_copying_wide_identity() {
    let mut query = query("WITH q AS (SELECT 1) SELECT 7");
    query.ctes[0].name = "x".repeat(64 * 1024);
    let mut plan = UnifiedPlan::Query(query);
    let before = serde_json::to_value(&plan).unwrap();
    let memory = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = ProductionControl::new(&memory, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(prune(&mut plan, &config, &|_: &str| false));
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(memory.peak() > 0);
    assert_eq!(memory.used(), 0);
    assert_eq!(serde_json::to_value(&plan).unwrap(), before);
}

#[test]
fn positional_reference_membership_preserves_hidden_aliases_and_partial_copy_cleanup() {
    let relation = InternalRelationId::allocate();
    let base =
        RowSchema::with_internal_relation_types(relation, vec![Some(ColumnType::Integer); 2]);
    let schema = RowSchema::with_physical_identity_aliases(
        &base,
        &[
            (
                uqa_sql::ColumnIdentity::qualified("q", "a"),
                0,
                Some(ColumnType::Integer),
            ),
            (
                uqa_sql::ColumnIdentity::qualified("q", "b"),
                1,
                Some(ColumnType::Integer),
            ),
            (
                uqa_sql::ColumnIdentity::qualified("q", "repeated"),
                1,
                Some(ColumnType::Integer),
            ),
        ],
    );
    let parent = MemoryBudget::new(4096);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(2048);
    let token = CancellationToken::new();
    let mut demand = Demand::new(ProductionControl::new(&memory, &token, &token));
    references::References::new(&schema, &mut demand)
        .expression(&ScalarExpr::QualifiedStar("q".into()), &[])
        .unwrap();
    assert_eq!(demand.iter().copied().collect::<Vec<_>>(), [0, 1]);
    let retained = memory.used();
    references::References::new(&schema, &mut demand)
        .expression(&ScalarExpr::Column("q".into()), &[])
        .unwrap();
    assert_eq!(memory.used(), retained);
    let occupied = memory.reserve(memory.available() - retained / 2).unwrap();
    let Err(error) = demand.copy() else {
        panic!("partial copy must be rejected");
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), retained + occupied.bytes());
    drop(occupied);
    let copied = demand.copy().unwrap();
    assert_eq!(memory.used(), 2 * retained);
    drop(copied);
    drop(demand);
    assert_eq!(memory.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn positional_cte_frames_preserve_visibility_and_retire_with_their_owners() {
    let outer = query("WITH \"q\"\"r\" AS (SELECT 1) SELECT 1");
    let inner = query("WITH a AS (SELECT 2), \"q\"\"r\" AS (SELECT 3) SELECT 1");
    let parent = MemoryBudget::new(16384);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let root = ctes::Scope::command(&outer.ctes, control, None).unwrap();
    let root_memory = memory.used();
    let scope = ctes::Scope::command(&inner.ctes, control, Some(&root)).unwrap();
    let reference = RelationIdentity::borrowed_reference("\"q\"\"r\"").unwrap();
    assert!(std::ptr::eq(
        scope.find(reference.name).unwrap(),
        scope.local("q\"r")
    ));
    scope.set_visible(&inner.ctes, 1, false, control).unwrap();
    assert!(std::ptr::eq(
        scope.find(reference.name).unwrap(),
        root.local("q\"r")
    ));
    scope.set_visible(&inner.ctes, 0, true, control).unwrap();
    assert!(std::ptr::eq(
        scope.find(reference.name).unwrap(),
        scope.local("q\"r")
    ));
    drop(scope);
    assert_eq!(memory.used(), root_memory);
    drop(root);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn positional_cte_definition_pruning_borrows_completed_demand() {
    let mut query = query("WITH q AS (SELECT 1 AS kept) SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut query.ctes[0].body.query_mut().unwrap().root
    else {
        panic!("CTE block");
    };
    block.projections.resize(1024, block.projections[0].clone());
    let memory = MemoryBudget::new(256 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = control;
    let scope = ctes::Scope::command(&query.ctes, control, None).unwrap();
    let mut positions = Demand::new(control);
    positions.extend(0..1024).unwrap();
    let demand = scope.local("q");
    *demand.positions.borrow_mut() = Some(positions);
    demand.observed.set(true);
    let before = memory.used();
    let context = Context {
        config: &config,
        aggregates: &|_: &str| false,
        ctes: Some(&scope),
    };
    let allocation = allocation_counter::measure(|| {
        context.cte_definitions(&mut query, &[0]).unwrap();
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(memory.used(), before);
    drop(scope);
    assert_eq!(memory.used(), 0);
}
