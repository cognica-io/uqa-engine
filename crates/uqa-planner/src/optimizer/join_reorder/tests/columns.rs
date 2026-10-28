//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn relation(alias: &str, columns: impl IntoIterator<Item = String>) -> RelationMetadata<'static> {
    RelationMetadata {
        paradigm: crate::AccessParadigm::Relational,
        alias: alias.to_owned().into(),
        cardinality: 100.0,
        access_cost: 100.0,
        column_stats: columns
            .into_iter()
            .map(|column| (column, crate::ColumnStats::default()))
            .collect(),
    }
}

#[test]
fn column_resolution_borrows_existing_names_without_index_storage() {
    let column = "x".repeat(4 * 1024 * 1024);
    let relations = [relation("a", [column])];
    let name = relations[0].column_stats.first_key_value().unwrap().0;
    let allocation = allocation_counter::measure(|| {
        let owners = ColumnOwners::new(&relations);
        let (alias, column) = owners.resolve(name).unwrap();
        assert!(std::ptr::eq(alias.as_ptr(), relations[0].alias.as_ptr()));
        assert!(std::ptr::eq(column.as_ptr(), name.as_ptr()));
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}

#[test]
fn column_resolution_preserves_unique_ambiguous_and_unknown_names() {
    for count in [1, 2, 8, 64] {
        for width in [1, 64, 1024] {
            let mut relations: Vec<_> = (0..count)
                .map(|index| {
                    relation(
                        &format!("r{index}"),
                        std::iter::once(format!("unique_{index}"))
                            .chain((0..width).map(|column| format!("shared_{column}"))),
                    )
                })
                .collect();
            let unique: Vec<_> = (0..count).map(|index| format!("unique_{index}")).collect();
            for reversed in [false, true] {
                if reversed {
                    relations.reverse();
                }
                let allocation = allocation_counter::measure(|| {
                    let owners = ColumnOwners::new(&relations);
                    for (index, column) in unique.iter().enumerate() {
                        let relation = &relations[if reversed { count - index - 1 } else { index }];
                        let (alias, name) = owners.resolve(column).unwrap();
                        assert!(owners.contains_alias(relation.alias.as_ref()));
                        let key = relation.column_stats.get_key_value(column).unwrap().0;
                        assert_eq!(alias, relation.alias);
                        assert_eq!(name, column);
                        assert!(std::ptr::eq(alias.as_ptr(), relation.alias.as_ptr()));
                        assert!(std::ptr::eq(name.as_ptr(), key.as_ptr()));
                    }
                    assert_eq!(
                        owners.resolve("shared_0"),
                        (count == 1).then_some(("r0", "shared_0"))
                    );
                    assert!(owners.resolve("unknown").is_none());
                    assert!(owners.resolve("UNIQUE_0").is_none());
                    assert!(!owners.contains_alias("unknown"));
                    assert!(!owners.contains_alias("R0"));
                });
                assert_eq!(allocation.bytes_total, 0, "{count}/{width}: {allocation:?}");
            }
        }
    }
}

#[test]
fn rejected_join_search_does_not_copy_unused_column_names() {
    let plan = UnifiedPlan::lower(
        uqa_sql::compile("SELECT 1 FROM a CROSS JOIN b")
            .unwrap()
            .remove(0),
    );
    let column = "x".repeat(4 * 1024 * 1024);
    let columns = std::cell::RefCell::new(Some(BTreeMap::from([(
        column,
        crate::ColumnStats::default(),
    )])));
    let calls = std::cell::Cell::new(0);
    let statistics = |table: &str| {
        calls.set(calls.get() + 1);
        match table {
            "a" => Some(crate::RelationStats {
                row_count: 100,
                columns: columns.borrow_mut().take().unwrap().into(),
            }),
            "b" => None,
            _ => panic!("unexpected table"),
        }
    };
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        let error = reorder_unified_plan_joins(plan, &statistics, &control).unwrap_err();
        let crate::optimizer::OptimizerError::Expression(error) = error else {
            panic!("typed admission failure")
        };
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    assert_eq!(calls.get(), 2);
}

#[test]
fn local_filter_costs_keep_relation_order_and_stop_on_original_error() {
    struct Estimates {
        calls: std::cell::RefCell<Vec<usize>>,
        failure: bool,
    }
    impl SourceStatistics for Estimates {
        fn relation_statistics(&self, _: &str) -> Option<crate::RelationStats> {
            None
        }
        fn local_access_estimate(
            &self,
            table: &str,
            _: &ScalarExpr,
        ) -> Option<crate::LocalAccessEstimate> {
            let index = match table {
                "source_b" => 0,
                "source_a" => 1,
                "source_c" => 2,
                _ => panic!("unexpected table"),
            };
            self.calls.borrow_mut().push(index);
            Some(crate::LocalAccessEstimate {
                output_rows: if self.failure && index == 1 {
                    f64::NAN
                } else {
                    10.0 + index as f64
                },
                cost: 20.0 + index as f64,
                paradigm: crate::AccessParadigm::Text,
            })
        }
    }
    let relations = [relation("b", []), relation("a", []), relation("c", [])];
    let tables = [Some("source_b"), Some("source_a"), Some("source_c")];
    let predicates = ["a", "b", "c"].map(|alias| ScalarExpr::QualifiedColumn {
        qualifier: alias.into(),
        column: "id".into(),
    });
    let inputs: Vec<_> = predicates.iter().collect();
    for failure in [false, true] {
        let estimates = Estimates {
            calls: std::cell::RefCell::new(Vec::with_capacity(3)),
            failure,
        };
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let outcome = estimate_local_filters(
            &relations,
            &inputs,
            &ColumnOwners::new(&relations),
            &tables,
            &estimates,
            &control,
        );
        assert_eq!(budget.used(), 0);
        if failure {
            assert_eq!(*estimates.calls.borrow(), [0, 1]);
            let crate::optimizer::OptimizerError::JoinGraph(JoinGraphError::InvalidCardinality {
                name,
                rows,
            }) = outcome.unwrap_err()
            else {
                panic!("original provider error");
            };
            assert_eq!(name, "a");
            assert!(rows.is_nan());
        } else {
            assert_eq!(*estimates.calls.borrow(), [0, 1, 2]);
            let output = outcome.unwrap();
            for (index, estimate) in output[..3].iter().enumerate() {
                assert_eq!(
                    *estimate,
                    Some(crate::LocalAccessEstimate {
                        output_rows: 10.0 + index as f64,
                        cost: 20.0 + index as f64,
                        paradigm: crate::AccessParadigm::Text,
                    })
                );
            }
            assert!(output[3..].iter().all(Option::is_none));
        }
        assert!(relations
            .iter()
            .all(|relation| relation.cardinality == 100.0 && relation.access_cost == 100.0));
    }
}
