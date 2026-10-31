//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod admission;
mod bindings;
mod columns;
mod costing;
mod inference;
mod materialization;
mod metadata;
mod scratch;

#[test]
fn unavailable_function_cost_does_not_copy_join_source_payloads() {
    for size in [4 * 1024 * 1024, 4096] {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(uqa_sql::compile(
            "SELECT * FROM regexp_split_to_table('a', ',') AS words(v) CROSS JOIN generate_series(1, 2) AS numbers(n)"
        ).unwrap().remove(0)) else { panic!("query"); };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("query block");
        };
        let source = block.from.as_mut().unwrap();
        let SourcePlan::Join { left, .. } = source else {
            panic!("join");
        };
        let SourcePlan::Function { args, .. } = left.as_mut() else {
            panic!("function");
        };
        args[0] = ScalarExpr::Literal(uqa_core::Value::Str("x".repeat(size)));
        let allocation = allocation_counter::measure(|| {
            let result = reordered_inner_join_source(
                source,
                &[],
                &|_: &str| None,
                &ProductionControl::uncontrolled(),
            )
            .unwrap();
            assert!(result.is_none());
        });
        assert!(allocation.bytes_total < 4096, "size={size}: {allocation:?}");
        assert_eq!(allocation.bytes_current, 0);
    }
}

#[test]
fn unavailable_function_cost_does_not_copy_join_predicates() {
    for where_clause in [false, true] {
        let mut plan = UnifiedPlan::lower(uqa_sql::compile(
            "SELECT * FROM generate_series(1, 2) AS a(n) JOIN generate_series(3, 4) AS b(n) ON a.n = b.n"
        ).unwrap().remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query");
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("query block");
        };
        let predicate = ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Str(
                "x".repeat(4 * 1024 * 1024),
            ))),
            rhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Str("other".into()))),
        };
        if where_clause {
            block.r#where = Some(predicate);
        } else {
            let SourcePlan::Join { on, .. } = block.from.as_mut().unwrap() else {
                panic!("join");
            };
            *on = Some(predicate);
        }
        let control = ProductionControl::uncontrolled();
        let mut rewrite = JoinRewrite::new(plan, &control);
        let allocation = allocation_counter::measure(|| {
            rewrite.run(&|_: &str| None, &control).unwrap();
        });
        assert!(
            allocation.bytes_total < 4096,
            "where={where_clause}: {allocation:?}"
        );
        assert_eq!(allocation.bytes_current, 0);
    }
}

#[test]
fn local_join_costing_borrows_predicates_and_column_statistics() {
    use std::cell::Cell;
    use uqa_core::Value;

    struct Estimates<'a> {
        predicate: &'a ScalarExpr,
        output: Option<crate::LocalAccessEstimate>,
        calls: Cell<usize>,
        borrowed: Cell<bool>,
    }
    impl SourceStatistics for Estimates<'_> {
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
            self.borrowed.set(std::ptr::eq(predicate, self.predicate));
            self.output
        }
    }

    for output in [
        None,
        Some(crate::LocalAccessEstimate {
            output_rows: 7.0,
            cost: 3.0,
            paradigm: crate::AccessParadigm::Text,
        }),
    ] {
        let large = Value::Str("x".repeat(4 * 1024 * 1024));
        let predicate = ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::QualifiedColumn {
                qualifier: "a".into(),
                column: "id".into(),
            }),
            rhs: Box::new(ScalarExpr::Literal(large.clone())),
        };
        let relations = [RelationMetadata {
            paradigm: crate::AccessParadigm::Relational,
            alias: "a".into(),
            cardinality: 100.0,
            access_cost: 100.0,
            column_stats: BTreeMap::from([(
                "id".into(),
                crate::ColumnStats {
                    row_count: 100,
                    distinct_count: 10,
                    mcv_values: vec![large],
                    mcv_frequencies: vec![0.25],
                    ..Default::default()
                },
            )])
            .into(),
        }];
        let owners = ColumnOwners::new(&relations);
        let tables = [Some("source_table")];
        let estimates = Estimates {
            predicate: &predicate,
            output,
            calls: Cell::new(0),
            borrowed: Cell::new(false),
        };
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(
                estimate_local_filters(
                    &relations,
                    &[&predicate],
                    &owners,
                    &tables,
                    &estimates,
                    &ProductionControl::uncontrolled(),
                )
                .unwrap()[0]
                    .unwrap(),
            );
        });
        let result = result.unwrap();
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(estimates.calls.get(), 1);
        assert!(estimates.borrowed.get());
        let expected = output.unwrap_or_else(|| crate::LocalAccessEstimate {
            paradigm: crate::AccessParadigm::Relational,
            output_rows: 25.0,
            cost: 100.0
                + crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::Filter, 100.0)
                    .total(),
        });
        assert_eq!(result, expected);
    }
}

#[derive(Debug)]
struct SelectedSource {
    source: SourcePlan,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}
impl std::ops::Deref for SelectedSource {
    type Target = SourcePlan;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

fn reordered_inner_join_source(
    source: &mut SourcePlan,
    external: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<Option<SelectedSource>> {
    let mut output = Output::new(control);
    let source = rewrite_inner_join_source(source, external, statistics, control, &mut output)?;
    Ok(source.map(|source| SelectedSource {
        source,
        _memory: output.finish(),
    }))
}

mod output;
