//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn rejected_join_search_borrows_relation_names_before_admission() {
    for selected in 0..3 {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
            uqa_sql::compile("SELECT 1 FROM a CROSS JOIN b")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let source = block.from.as_mut().unwrap();
        let SourcePlan::Join { left, .. } = source else {
            panic!("join")
        };
        let SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } = left.as_mut()
        else {
            panic!("table")
        };
        let payload = "x".repeat(4 * 1024 * 1024);
        let pointer = payload.as_ptr();
        match selected {
            0 => *alias = Some(payload),
            1 => *qualifier = payload,
            _ => *name = payload,
        }
        let calls = std::cell::Cell::new(0);
        let statistics = |_: &str| {
            calls.set(calls.get() + 1);
            None
        };
        let budget = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let allocation = allocation_counter::measure(|| {
            let error =
                reordered_inner_join_source(source, &[], &statistics, &control).unwrap_err();
            let crate::optimizer::OptimizerError::Expression(error) = error else {
                panic!("admission failure")
            };
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(
            allocation.bytes_total < 16 * 1024,
            "field={selected}: {allocation:?}"
        );
        assert_eq!(budget.used(), 0);
        assert_eq!(calls.get(), 2);
        let SourcePlan::Join { left, .. } = source else {
            panic!("original join")
        };
        let SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } = left.as_ref()
        else {
            panic!("original table")
        };
        let retained = match selected {
            0 => alias.as_ref().unwrap(),
            1 => qualifier,
            _ => name,
        };
        assert_eq!(retained.as_ptr(), pointer);
    }
}

#[test]
fn declined_join_metadata_borrows_names_and_releases_captured_statistics() {
    struct Statistics {
        columns: std::sync::Arc<BTreeMap<String, crate::ColumnStats>>,
        calls: std::cell::Cell<usize>,
    }
    impl SourceStatistics for Statistics {
        fn relation_statistics(&self, name: &str) -> Option<crate::RelationStats> {
            assert_eq!(name, "a");
            assert_eq!(self.calls.replace(1), 0);
            Some(crate::RelationStats {
                row_count: 1,
                columns: self.columns.clone().into(),
            })
        }
        fn source_access_estimate(&self, _: &SourcePlan) -> Option<crate::LocalAccessEstimate> {
            assert_eq!(self.calls.replace(2), 1);
            assert_eq!(std::sync::Arc::strong_count(&self.columns), 2);
            None
        }
    }
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT 1 FROM a CROSS JOIN generate_series(1, 2) AS numbers(n)")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let source = block.from.as_mut().unwrap();
    let SourcePlan::Join { left, .. } = source else {
        panic!("join")
    };
    let SourcePlan::Table { alias, .. } = left.as_mut() else {
        panic!("table")
    };
    *alias = Some("x".repeat(4 * 1024 * 1024));
    let statistics = Statistics {
        columns: std::sync::Arc::new(BTreeMap::from([(
            "id".into(),
            crate::ColumnStats {
                min_value: Some(uqa_core::Value::Str("x".repeat(4 * 1024 * 1024))),
                ..Default::default()
            },
        )])),
        calls: std::cell::Cell::new(0),
    };
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        assert!(
            reordered_inner_join_source(source, &[], &statistics, &control)
                .unwrap()
                .is_none()
        );
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(statistics.calls.get(), 2);
    assert_eq!(std::sync::Arc::strong_count(&statistics.columns), 1);
    assert_eq!(budget.used(), 0);
}
