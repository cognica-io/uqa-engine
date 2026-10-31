//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::empty_scope;
use uqa_sql::catalog::resolution::RelationLookupMode;

#[test]
fn source_metadata_requires_the_same_catalog_and_namespace() {
    let scope = empty_scope();
    let inputs =
        SourceBindingScope::capture(&scope, &uqa_core::memory::ProductionControl::uncontrolled())
            .unwrap();
    assert!(inputs.matches(&scope));
    assert!(inputs.matches(&scope.clone()));
    assert!(!inputs.matches(&empty_scope()));
    let mut bound = scope.clone();
    bound
        .set_relation_lookup_mode(RelationLookupMode::Bound)
        .unwrap();
    assert!(inputs.matches(&bound));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .ordinary_mut()
        .search_path
        .insert(0, "other".into());
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .ordinary_mut()
        .temporary_namespace_allocated = true;
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed
        .catalog_resolution
        .as_mut()
        .unwrap()
        .ordinary_mut()
        .temporary_schema
        .push_str("_other");
    assert!(!inputs.matches(&changed));
}

#[test]
fn source_metadata_does_not_cross_cte_definitions_or_recursive_descriptors() {
    let mut scope = empty_scope();
    let uqa_sql::plan::UnifiedPlan::Query(mut query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("WITH input AS (SELECT 1 AS x) SELECT * FROM input")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    scope.insert_deferred(query.ctes.remove(0).into());
    let inputs =
        SourceBindingScope::capture(&scope, &uqa_core::memory::ProductionControl::uncontrolled())
            .unwrap();
    assert!(inputs.matches(&scope.clone()));
    let mut changed = scope.clone();
    let original = changed.deferred_ctes.get("input").unwrap();
    changed
        .deferred_ctes
        .insert("input".into(), (**original).clone().into());
    assert!(!inputs.matches(&changed));
    let mut changed = scope.clone();
    changed.recursive_control_widths.insert("input".into(), 1);
    assert!(!inputs.matches(&changed));
    let mut changed = scope;
    changed.non_returning_ctes.insert("input".into());
    assert!(!inputs.matches(&changed));
}

#[test]
fn source_binding_capture_shares_native_namespace_and_retains_the_last_reader() {
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    let input = empty_scope();
    let mut namespace = input.relation_name_resolution().unwrap();
    namespace.search_path.push("n".repeat(4 << 20));
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let scope = CteScope::<()>::with_catalog_with_control(
        input.catalog_read_view().unwrap(),
        namespace.copy_with_control(&control).unwrap(),
        &control,
    )
    .unwrap();
    let mut captured = None;
    let allocation = allocation_counter::measure(|| {
        captured = Some(SourceBindingScope::capture(&scope, &control).unwrap());
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    let captured = captured.unwrap();
    assert!(captured.matches(&scope));
    drop(scope);
    assert!(budget.used() >= 4 << 20);
    drop(captured);
    assert_eq!(budget.used(), 0);
}

#[test]
fn source_binding_capture_admits_lexical_names_without_replacing_raw_row_schemas() {
    use crate::{query::cte::rows::CteRows, TableScan};
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    let name = "n".repeat(4 << 20);
    let uqa_sql::plan::UnifiedPlan::Query(mut query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("WITH input AS (SELECT 1 AS x) SELECT * FROM input")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let definition: uqa_sql::plan::RetainedCtePlan = query.ctes.remove(0).into();
    for field in 0..4 {
        let mut scope = empty_scope();
        match field {
            0 => {
                scope.rows.insert(
                    name.clone(),
                    CteRows::from_producer(
                        Box::new(TableScan::from_physical_rows(
                            RowSchema::new(vec!["value".into(), "generated".into()]),
                            Vec::new(),
                        )),
                        1024,
                    ),
                );
                scope.recursive_control_widths.insert(name.clone(), 1);
            }
            1 => {
                scope.deferred_ctes.insert(name.clone(), definition.clone());
            }
            2 => {
                scope.non_returning_ctes.insert(name.clone());
            }
            _ => {
                scope.recursive_control_widths.insert(name.clone(), 1);
            }
        }
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
        let mut error = None;
        let allocation = allocation_counter::measure(|| {
            error = SourceBindingScope::capture(&scope, &control).err();
        });
        assert_eq!(error.unwrap().sqlstate(), Some("53200"));
        assert!(
            allocation.bytes_total < 16 << 10,
            "field {field}: {allocation:?}"
        );
        assert_eq!(budget.used(), occupied.bytes());
        drop(occupied);
        let captured = SourceBindingScope::capture(&scope, &control).unwrap();
        assert!(captured.matches(&scope));
        if field == 0 {
            assert_eq!(
                captured.rows.get(&name).unwrap().columns(),
                &["value", "generated"]
            );
            assert_eq!(captured.recursive_widths.get(&name), Some(&1));
        }
        let admitted = budget.used();
        assert!(admitted >= name.len());
        drop(captured);
        assert_eq!(budget.used(), 0);
        let occupied = budget.reserve(budget.available() - (admitted - 1)).unwrap();
        assert_eq!(
            SourceBindingScope::capture(&scope, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), occupied.bytes());
        drop(occupied);
        let captured = SourceBindingScope::capture(&scope, &control).unwrap();
        let mut last_reader = None;
        let allocation = allocation_counter::measure(|| last_reader = Some(captured.clone()));
        assert_eq!(allocation.bytes_total, 0);
        scope.non_returning_ctes.insert("another".into());
        assert!(!captured.matches(&scope));
        drop(captured);
        drop(scope);
        assert_eq!(budget.used(), admitted);
        drop(last_reader);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn source_binding_capture_releases_on_cancellation_and_unwind() {
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    let mut scope = empty_scope();
    scope.non_returning_ctes.insert("command".into());
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            SourceBindingScope::capture(&scope, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _captured = SourceBindingScope::capture(&scope, &control).unwrap();
        panic!("retire private source bindings");
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.used(), 0);
}
