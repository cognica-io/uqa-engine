//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::plan::{SharedExecutablePlan, UnifiedPlan};

fn fixture() -> (Arc<uqa_sql::Statement>, SharedExecutablePlan) {
    let statement = uqa_sql::compile("SELECT 7").unwrap().remove(0);
    let plan = UnifiedPlan::lower(statement.clone()).into();
    (Arc::new(statement), plan)
}

fn publish(
    cache: &mut SQLStatementCache,
    sql: &str,
    statement: &Arc<uqa_sql::Statement>,
    plan: &SharedExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    cache.insert(
        sql,
        SQLStatementInput {
            statement: statement.clone(),
            logical_plan: plan.clone(),
            catalog_epochs: CatalogEpochs {
                table_catalog: 1,
                table_data: 1,
                catalog_registry: 1,
            },
            function_registry_revision: 1,
            parser: uqa_sql::parser::ParserMetadata::default(),
        },
        control,
    )
}

#[test]
fn snapshots_share_leases_and_failed_writes_preserve_both_roots() {
    let budget = MemoryBudget::new(1 << 20);
    let writer_budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let writer = ProductionControl::new(&writer_budget, &original, &invoking);
    let (statement, plan) = fixture();
    let mut cache = SQLStatementCache::default();
    publish(&mut cache, "one", &statement, &plan, &control).unwrap();
    let retained = budget.used();
    let mut snapshot = None;
    let allocation = allocation_counter::measure(|| snapshot = Some(cache.clone()));
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(budget.used(), retained);
    let snapshot = snapshot.unwrap();

    // Permit the shared-state header and its first node, then reject the copy.
    let occupied = writer_budget
        .reserve(
            writer_budget.available()
                - size_of::<CacheState>()
                - OwnedMap::<CacheKey, CachedSQLStatement>::entry_bytes(),
        )
        .unwrap();
    assert_eq!(
        publish(&mut cache, "two", &statement, &plan, &writer)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(Arc::ptr_eq(
        cache.state.as_ref().unwrap(),
        snapshot.state.as_ref().unwrap()
    ));
    assert!(cache.get("one").unwrap().logical_plan.ptr_eq(&plan));
    assert!(cache.get("two").is_none());
    assert_eq!(writer_budget.used(), occupied.bytes());
    assert_eq!(budget.used(), retained);
    drop(occupied);

    publish(&mut cache, "two", &statement, &plan, &writer).unwrap();
    assert!(snapshot.get("two").is_none());
    assert!(cache.get("two").is_some());
    assert_eq!(budget.used(), retained);
    drop(snapshot);
    // The original key is still shared by the writer; its nodes were replaced.
    assert!(budget.used() > 0 && budget.used() < retained);
    cache.clear();
    assert_eq!(budget.used(), 0);
    assert_eq!(writer_budget.used(), 0);
}

#[test]
fn insertion_rejection_does_not_evict_or_leave_an_order_node() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let (statement, plan) = fixture();
    let mut cache = SQLStatementCache::default();
    for position in 0..SQL_STATEMENT_CACHE_LIMIT {
        publish(
            &mut cache,
            &position.to_string(),
            &statement,
            &plan,
            &control,
        )
        .unwrap();
    }
    let before = budget.used();
    // The text, its shared owner and order node fit; the entry node does not.
    let allowance = 4 + size_of::<Produced<String>>() + OwnedMap::<u64, CacheKey>::entry_bytes();
    let occupied = budget.reserve(budget.available() - allowance).unwrap();
    assert_eq!(
        publish(&mut cache, "next", &statement, &plan, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), before + occupied.bytes());
    assert!(cache.get("0").is_some());
    assert!(cache.get("next").is_none());
    assert_eq!(
        cache.state.as_ref().unwrap().insertion_order.len(),
        SQL_STATEMENT_CACHE_LIMIT
    );
    drop(occupied);
    publish(&mut cache, "next", &statement, &plan, &control).unwrap();
    assert!(cache.get("0").is_none());
    assert!(cache.get("1").is_some());
    assert!(cache.get("next").is_some());
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn invalidation_and_rollback_retain_independent_optimized_observations() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let (statement, plan) = fixture();
    let mut cache = SQLStatementCache::default();
    publish(&mut cache, "one", &statement, &plan, &control).unwrap();
    cache
        .set_optimized("one", plan.clone(), 2, &control)
        .unwrap();
    let snapshot = cache.clone();
    let before = budget.used();
    let allocation = allocation_counter::measure(|| cache.invalidate_optimized());
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(budget.used(), before);
    assert!(cache.get("one").unwrap().optimized_plan.is_none());
    assert!(snapshot
        .get("one")
        .unwrap()
        .optimized_plan
        .unwrap()
        .ptr_eq(&plan));
    cache
        .set_optimized("one", plan.clone(), 3, &control)
        .unwrap();
    assert_eq!(snapshot.get("one").unwrap().catalog_epochs.table_data, 2);
    assert_eq!(cache.get("one").unwrap().catalog_epochs.table_data, 3);
    cache = snapshot;
    assert_eq!(cache.get("one").unwrap().catalog_epochs.table_data, 2);
    assert_eq!(budget.used(), before);
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn either_cancellation_prevents_publication_and_releases_no_existing_lease() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let (statement, plan) = fixture();
        let mut cache = SQLStatementCache::default();
        publish(&mut cache, "one", &statement, &plan, &control).unwrap();
        let before = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            publish(&mut cache, "two", &statement, &plan, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert!(cache.get("one").is_some());
        assert!(cache.get("two").is_none());
        assert_eq!(budget.used(), before);
        cache.clear();
        assert_eq!(budget.used(), 0);
    }
}
