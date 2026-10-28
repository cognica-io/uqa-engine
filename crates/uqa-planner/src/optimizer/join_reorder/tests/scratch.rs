//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn relation(alias: String, source_id: u64) -> JoinRelation {
    JoinRelation {
        alias,
        source_id,
        cardinality: 100.0,
        access_cost: 100.0,
        column_stats: BTreeMap::new(),
        paradigm: crate::AccessParadigm::Relational,
    }
}

fn predicate(sql: &str) -> ScalarExpr {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT 1 WHERE {sql}"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    block.r#where.unwrap()
}

#[test]
fn join_source_discovery_keeps_the_relation_bound_without_heap_scratch() {
    for count in [2, 8, 64, 65] {
        let sql = format!(
            "SELECT 1 FROM {}",
            (0..count)
                .map(|index| format!("t{index}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let source = block.from.unwrap();
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        control.check().unwrap();
        let allocation = allocation_counter::measure(|| {
            let mut atoms = ProductionSmallVec::<[&SourcePlan; 64]>::new(control);
            let mut predicates = ProductionVec::new(control);
            assert_eq!(
                flatten_reorderable_inner_join(&source, &mut atoms, &mut predicates, &control)
                    .unwrap(),
                count <= 64
            );
            assert_eq!(atoms.len(), count.min(64));
            assert!(predicates.is_empty());
        });
        assert_eq!(allocation.bytes_total, 0);
        for token in [&original, &invoking] {
            token.cancel();
            let mut atoms = ProductionSmallVec::<[&SourcePlan; 64]>::new(control);
            let mut predicates = ProductionVec::new(control);
            assert_eq!(
                flatten_reorderable_inner_join(&source, &mut atoms, &mut predicates, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert!(atoms.is_empty() && predicates.is_empty());
            token.reset();
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn local_predicate_groups_retain_exact_buffers_and_conjunct_order() {
    for count in [2, 8, 64] {
        let relations: Vec<_> = (0..count)
            .map(|i| relation(format!("a{i}"), i as u64))
            .collect();
        let aliases = relations
            .iter()
            .map(|relation| relation.alias.clone())
            .collect();
        let owners = ColumnOwners::new(&relations);
        let predicates: Vec<_> = (0..17 * count)
            .map(|i| ScalarExpr::QualifiedColumn {
                qualifier: format!("a{}", i % count),
                column: format!("id{i}").into(),
            })
            .collect();
        let inputs: Vec<_> = predicates.iter().collect();
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut groups = None;
        let allocation = allocation_counter::measure(|| {
            groups = Some(
                local_predicate_groups(&relations, &inputs, &aliases, &owners, &control).unwrap(),
            );
        });
        assert_eq!(allocation.bytes_current as usize, budget.used());
        assert!(budget.used() > 0);
        let groups = groups.unwrap();
        for (index, group) in groups.iter().take(count).enumerate() {
            let group = group.as_ref().unwrap();
            assert_eq!(group.len(), 17);
            for (position, value) in group.iter().enumerate() {
                assert!(std::ptr::eq(
                    *value,
                    &raw const predicates[position * count + index]
                ));
            }
        }
        let required = budget.peak();
        drop(groups);
        assert_eq!(budget.used(), 0);
        let parent = MemoryBudget::new(required);
        let minimum_payload = inputs.len() * size_of::<&ScalarExpr>();
        let sibling_bytes = required - (minimum_payload - 1);
        let sibling = parent.reserve(sibling_bytes).unwrap();
        let budget = parent.child(required);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = local_predicate_groups(&relations, &inputs, &aliases, &owners, &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling_bytes);
        drop(sibling);
        drop(local_predicate_groups(&relations, &inputs, &aliases, &owners, &control).unwrap());
        assert_eq!(parent.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            let error = local_predicate_groups(&relations, &inputs, &aliases, &owners, &control)
                .err()
                .unwrap();
            assert_eq!(error.sqlstate(), Some("57014"));
            token.reset();
        }
        assert_eq!(parent.used(), 0);
    }
}

#[test]
fn local_predicate_groups_preserve_resolution_and_expression_restrictions() {
    let mut relations = [
        relation("b".into(), 0),
        relation("a".into(), 1),
        relation("c".into(), 2),
    ];
    let aliases = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
    relations[1]
        .column_stats
        .insert("local_a".into(), crate::ColumnStats::default());
    for relation in &mut relations[..2] {
        relation
            .column_stats
            .insert("ambiguous".into(), crate::ColumnStats::default());
    }
    let owners = ColumnOwners::new(&relations);
    let predicates: Vec<_> = [
        "a.id = 1",
        "b.id = 2",
        "a.id > b.id",
        "local_a = 4",
        "ambiguous = 5",
        "foreign_source.id = 1",
        "true",
        "a.id = 7 OR a.id = 8",
        "a.id = 7 OR b.id = 8",
        "CASE WHEN a.id IS NULL THEN 0 ELSE a.id END > 0",
        "coalesce(a.id, 1) = 2",
        "a.id IN (1, 2)",
        "a.id = (SELECT 1)",
        "a.id BETWEEN 1 AND 3",
    ]
    .into_iter()
    .map(predicate)
    .collect();
    let inputs: Vec<_> = predicates.iter().collect();
    let groups = local_predicate_groups(
        &relations,
        &inputs,
        &aliases,
        &owners,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    for (group, expected) in
        groups
            .iter()
            .take(3)
            .zip([&[1][..], &[0, 3, 7, 9, 10, 11, 13][..], &[][..]])
    {
        let group = group.as_ref().unwrap();
        assert_eq!(group.len(), expected.len());
        for (value, index) in group.iter().zip(expected) {
            assert!(std::ptr::eq(*value, &raw const predicates[*index]));
        }
    }
}

fn wide_inputs(plan: &UnifiedPlan, where_clause: bool) -> &[ScalarExpr] {
    let UnifiedPlan::Query(query) = plan else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let predicate = if where_clause {
        block.r#where.as_ref()
    } else {
        let SourcePlan::Join { on, .. } = block.from.as_ref().unwrap() else {
            panic!("join")
        };
        on.as_ref()
    };
    let Some(ScalarExpr::And(items)) = predicate else {
        panic!("conjunction")
    };
    items
}

fn wide_predicate_rejection(where_clause: bool) {
    let mut plan = UnifiedPlan::lower(
        uqa_sql::compile("SELECT a.id FROM a JOIN b ON a.id = b.id")
            .unwrap()
            .remove(0),
    );
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let predicate = ScalarExpr::And(
        (0..65536)
            .map(|_| ScalarExpr::Literal(Value::Bool(true)))
            .collect(),
    );
    if where_clause {
        block.r#where = Some(predicate);
    } else {
        let SourcePlan::Join { on, .. } = block.from.as_mut().unwrap() else {
            panic!("join")
        };
        *on = Some(predicate);
    }
    let pointer = wide_inputs(&plan, where_clause).as_ptr();
    for (limit, cancelled) in [(0, 0), (1024, 0), (16384, 0), (0, 1), (0, 2)] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancelled == 1 {
            original.cancel();
        }
        if cancelled == 2 {
            invoking.cancel();
        }
        // Initialize the cancellation service before measuring planner-owned scratch.
        let _ = original.check();
        let _ = invoking.check();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let allocation = allocation_counter::measure(|| {
            let error =
                reorder_unified_plan_joins(&mut plan, &|_: &str| None, &control).unwrap_err();
            let crate::optimizer::OptimizerError::Expression(error) = error else {
                panic!("typed admission")
            };
            assert_eq!(
                error.sqlstate(),
                Some(if cancelled == 0 { "53200" } else { "57014" })
            );
        });
        assert!(
            allocation.bytes_total < 4 * limit.max(4096) as u64,
            "where={where_clause}, limit={limit}: {allocation:?}"
        );
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(budget.used(), 0);
        let items = wide_inputs(&plan, where_clause);
        assert_eq!(items.as_ptr(), pointer);
        assert_eq!(items.len(), 65536);
        assert!(items
            .iter()
            .all(|item| matches!(item, ScalarExpr::Literal(Value::Bool(true)))));
    }
}

#[test]
fn non_join_sources_do_not_collect_unused_where_predicates() {
    for sql in ["SELECT 1", "SELECT id FROM a"] {
        let mut plan = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.r#where = Some(ScalarExpr::And(
            (0..65536)
                .map(|_| ScalarExpr::Literal(Value::Bool(true)))
                .collect(),
        ));
        let budget = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let allocation = allocation_counter::measure(|| {
            reorder_unified_plan_joins(&mut plan, &|_: &str| None, &control).unwrap();
        });
        assert_eq!(allocation.bytes_total, 0);
        assert_eq!(budget.used(), 0);
        assert_eq!(wide_inputs(&plan, true).len(), 65536);
    }
}

#[test]
fn wide_where_predicate_collection_obeys_request_admission() {
    wide_predicate_rejection(true);
}

#[test]
fn wide_on_predicate_collection_obeys_request_admission() {
    wide_predicate_rejection(false);
}

#[test]
fn local_predicate_grouping_borrows_aliases_and_admits_reference_buffers() {
    let alias = "a".repeat(4 * 1024 * 1024);
    let predicate = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: alias.clone(),
            column: "id".into(),
        }),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
    };
    let relations = [JoinRelation {
        alias: alias.clone(),
        cardinality: 100.0,
        access_cost: 100.0,
        column_stats: BTreeMap::new(),
        paradigm: crate::AccessParadigm::Relational,
        source_id: 0,
    }];
    let aliases = BTreeSet::from([alias]);
    let owners = ColumnOwners::new(&relations);
    let tables = BTreeMap::new();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(estimate_local_filters(
            &relations,
            &[&predicate],
            &aliases,
            &owners,
            &tables,
            &|_: &str| None,
            &control,
        ));
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    let crate::optimizer::OptimizerError::Expression(error) = result.unwrap().unwrap_err() else {
        panic!("typed admission")
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert_eq!(relations[0].cardinality, 100.0);
}
