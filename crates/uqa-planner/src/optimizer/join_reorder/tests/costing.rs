//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn rejected_compound_join_costing_does_not_copy_large_literals() {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT a.id FROM a JOIN b ON a.id = b.id")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block");
    };
    let source = block.from.as_mut().unwrap();
    let predicates = [
        equality("payload", Value::Str("x".repeat(4 * 1024 * 1024))),
        equality("id", Value::Int(1)),
    ];
    for failure in 0..3 {
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if failure == 1 {
            original.cancel();
        } else if failure == 2 {
            invoking.cancel();
        }
        // Initialize cancellation-owner checks outside the planner's allocation measurement.
        let _ = original.check();
        let _ = invoking.check();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let allocation = allocation_counter::measure(|| {
            let error = reordered_inner_join_source(
                source,
                &[&predicates[0], &predicates[1]],
                &|_: &str| None,
                &control,
            )
            .unwrap_err();
            let crate::optimizer::OptimizerError::Expression(error) = error else {
                panic!("untyped admission failure: {error}");
            };
            assert_eq!(
                error.sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
        });
        assert!(
            allocation.bytes_total < 16 * 1024,
            "{failure}: {allocation:?}"
        );
        assert_eq!(allocation.bytes_current, 0, "{failure}: {allocation:?}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn compound_join_predicates_retain_exact_storage_and_release_partial_copies() {
    let predicates = [
        equality("id", Value::Int(1)),
        equality("payload", Value::Str("x".repeat(4 * 1024 * 1024))),
    ];
    let inputs = [&predicates[0], &predicates[1]];
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(copy_local_conjunction(&inputs, &control).unwrap());
    });
    let output = output.unwrap();
    let ScalarExpr::And(items) = &*output else {
        panic!("conjunction");
    };
    assert_eq!(items, &predicates);
    assert_eq!(allocation.bytes_current as usize, output.reserved_bytes());
    assert_eq!(budget.used(), output.reserved_bytes());
    assert!(
        allocation.bytes_total < 4 * 1024 * 1024 + 16 * 1024,
        "{allocation:?}"
    );
    assert!(allocation.bytes_max as usize <= budget.peak());
    let required = budget.peak();
    drop(output);
    assert_eq!(budget.used(), 0);

    for limit in [0, 1, required / 2, required - 1, required] {
        let parent = MemoryBudget::new(required);
        let sibling = parent.reserve(required - limit).unwrap();
        let budget = parent.child(required);
        let control = ProductionControl::new(&budget, &token, &token);
        let output = copy_local_conjunction(&inputs, &control);
        if limit < required {
            assert_eq!(output.unwrap_err().sqlstate(), Some("53200"), "{limit}");
        } else {
            let output = output.unwrap();
            let ScalarExpr::And(items) = &*output else {
                panic!("conjunction");
            };
            assert_eq!(items, &predicates);
        }
        assert_eq!(budget.used(), 0, "{limit}");
        assert_eq!(parent.used(), required - limit, "{limit}");
        drop(sibling);
        assert_eq!(parent.used(), 0);
    }
    let ordinary = copy_local_conjunction(&inputs, &ProductionControl::uncontrolled()).unwrap();
    assert_eq!(ordinary.reserved_bytes(), 0);
    assert_eq!(*ordinary, ScalarExpr::And(predicates.to_vec()));
}

struct CompoundEstimates<'a> {
    expected: &'a [ScalarExpr],
    memory: &'a MemoryBudget,
    mode: usize,
    calls: Cell<usize>,
}
impl SourceStatistics for CompoundEstimates<'_> {
    fn relation_statistics(&self, _: &str) -> Option<crate::RelationStats> {
        None
    }

    fn local_access_estimate(
        &self,
        table: &str,
        predicate: &ScalarExpr,
    ) -> Option<crate::LocalAccessEstimate> {
        assert_eq!(table, "source_table");
        self.calls.set(self.calls.get() + 1);
        let ScalarExpr::And(items) = predicate else {
            panic!("combined provider predicate");
        };
        assert_eq!(items, self.expected);
        assert!(self.memory.used() > 128);
        assert_ne!(items.as_ptr(), self.expected.as_ptr());
        assert!(self.mode != 4, "provider unwind");
        (self.mode != 0).then_some(crate::LocalAccessEstimate {
            output_rows: if self.mode == 2 { f64::NAN } else { 7.0 },
            cost: if self.mode == 3 { -1.0 } else { 3.0 },
            paradigm: crate::AccessParadigm::Text,
        })
    }
}

#[test]
fn compound_join_costing_keeps_leases_through_estimates_errors_and_unwind() {
    let predicates = [
        equality("payload", Value::Str("x".repeat(128))),
        equality("id", Value::Int(1)),
    ];
    for mode in 0..5 {
        let budget = MemoryBudget::new(64 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let estimates = CompoundEstimates {
            expected: &predicates,
            memory: &budget,
            mode,
            calls: Cell::new(0),
        };
        let mut relations = [JoinRelation {
            alias: "a".into(),
            cardinality: 100.0,
            access_cost: 100.0,
            paradigm: crate::AccessParadigm::Relational,
            source_id: 0,
            column_stats: BTreeMap::from([
                (
                    "payload".into(),
                    crate::ColumnStats {
                        row_count: 100,
                        distinct_count: 10,
                        mcv_values: vec![Value::Str("x".repeat(128))],
                        mcv_frequencies: vec![0.4],
                        ..Default::default()
                    },
                ),
                (
                    "id".into(),
                    crate::ColumnStats {
                        row_count: 100,
                        distinct_count: 10,
                        ..Default::default()
                    },
                ),
            ]),
        }];
        let aliases = BTreeSet::from(["a".into()]);
        let owners = unique_column_owners(&relations);
        let tables = BTreeMap::from([("a".into(), "source_table".into())]);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_local_filter_estimates(
                &mut relations,
                &[&predicates[0], &predicates[1]],
                &aliases,
                &owners,
                &tables,
                &estimates,
                &control,
            )
        }));
        assert_eq!(estimates.calls.get(), 1);
        assert_eq!(budget.used(), 0, "mode={mode}");
        match mode {
            0 => {
                outcome.unwrap().unwrap();
                assert_eq!(relations[0].cardinality, 100.0 * (0.4 * 0.1));
                assert_eq!(
                    relations[0].access_cost,
                    100.0
                        + crate::CostEstimator::default()
                            .estimate_unary(crate::OperatorKind::Filter, 100.0)
                            .total()
                );
            }
            1 => {
                outcome.unwrap().unwrap();
                assert_eq!(relations[0].cardinality, 7.0);
                assert_eq!(relations[0].access_cost, 3.0);
                assert_eq!(relations[0].paradigm, crate::AccessParadigm::Text);
            }
            2 | 3 => {
                assert!(matches!(
                    outcome.unwrap().unwrap_err(),
                    crate::optimizer::OptimizerError::JoinGraph(_)
                ));
                assert_eq!(relations[0].cardinality, 100.0);
                assert_eq!(relations[0].access_cost, 100.0);
            }
            _ => assert!(outcome.is_err()),
        }
    }
}

fn equality(column: &str, value: Value) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "a".into(),
            column: column.into(),
        }),
        rhs: Box::new(ScalarExpr::Literal(value)),
    }
}
