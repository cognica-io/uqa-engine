use std::collections::BTreeSet;

use uqa_sql::compile;

use super::*;

fn optimized(sql: &str) -> UnifiedPlan {
    let mut statements = compile(sql).expect("SQL compiles");
    optimize(
        UnifiedPlan::lower(statements.remove(0)),
        &OptimizerConfig::default(),
    )
    .expect("optimizer succeeds")
}

fn optimized_with_rows(sql: &str, rows: &[(&str, u64)]) -> UnifiedPlan {
    let mut statements = compile(sql).expect("SQL compiles");
    let rows = rows.iter().copied().collect::<BTreeMap<_, _>>();
    optimize_with_statistics(
        UnifiedPlan::lower(statements.remove(0)),
        &OptimizerConfig::default(),
        &|table: &str| {
            rows.get(table).copied().map(|row_count| {
                let column = || crate::ColumnStats {
                    distinct_count: row_count,
                    row_count,
                    ..crate::ColumnStats::default()
                };
                crate::RelationStats::new(row_count)
                    .with_column("id", column())
                    .with_column("a_id", column())
                    .with_column("b_id", column())
            })
        },
    )
    .expect("optimizer succeeds")
}

fn query_block(plan: &UnifiedPlan) -> &QueryBlockPlan {
    let UnifiedPlan::Query(query) = plan else {
        panic!("query plan expected");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block expected");
    };
    block
}

fn source_aliases(source: &SourcePlan) -> BTreeSet<String> {
    match source {
        SourcePlan::Table { name, alias } => {
            BTreeSet::from([alias.clone().unwrap_or_else(|| name.clone())])
        }
        SourcePlan::Join { left, right, .. } => {
            let mut aliases = source_aliases(left);
            aliases.extend(source_aliases(right));
            aliases
        }
        SourcePlan::Values { .. } | SourcePlan::Function { .. } | SourcePlan::Subquery { .. } => {
            BTreeSet::new()
        }
    }
}

fn conjunct_count(expression: &ScalarExpr) -> usize {
    match expression {
        ScalarExpr::And(items) => items.iter().map(conjunct_count).sum(),
        _ => 1,
    }
}

fn source_predicate_count(source: &SourcePlan) -> usize {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            source_predicate_count(left)
                + source_predicate_count(right)
                + on.as_ref().map_or(0, conjunct_count)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => 0,
    }
}

#[test]
fn simplifies_boolean_expressions_after_lowering() {
    let UnifiedPlan::Query(query) = optimized("SELECT x FROM t WHERE true AND x = 1") else {
        panic!("query plan expected");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block expected");
    };
    assert!(matches!(
        block.r#where,
        Some(ScalarExpr::Binary {
            op: BinaryOp::Equal,
            ..
        })
    ));
}

#[test]
fn selects_operator_tree_access_and_pushes_relational_limit() {
    let UnifiedPlan::Query(query) = optimized(
        "SELECT id FROM docs WHERE text_match(body, 'rust') \
         ORDER BY _score DESC LIMIT 5",
    ) else {
        panic!("query plan expected");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block expected");
    };
    assert!(matches!(
        block.access,
        AccessPathPlan::OperatorTree {
            score_limit_pushdown: true
        }
    ));
}

#[test]
fn optimizes_mutation_and_cte_children() {
    let UnifiedPlan::Command(command) = optimized(
        "WITH q AS (SELECT 1 AS x WHERE true) \
         UPDATE t SET x = 1 + 2 WHERE true AND id = 1",
    ) else {
        panic!("command plan expected");
    };
    let CommandPlan::Update(update) = command.as_ref() else {
        panic!("update plan expected");
    };
    assert_eq!(update.ctes.len(), 1);
    assert!(matches!(
        update.predicate,
        Some(ScalarExpr::Binary {
            op: BinaryOp::Equal,
            ..
        })
    ));
}

#[test]
fn optimizes_query_bodies_owned_by_commands() {
    let UnifiedPlan::Command(command) = optimized(
        "PREPARE search AS SELECT id FROM docs \
         WHERE true AND text_match(body, 'rust') \
         ORDER BY _score DESC LIMIT 3",
    ) else {
        panic!("command plan expected");
    };
    let CommandPlan::Prepare { body, .. } = command.as_ref() else {
        panic!("prepare plan expected");
    };
    let UnifiedPlan::Query(query) = body.as_ref() else {
        panic!("prepared query expected");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block expected");
    };
    assert!(matches!(
        block.r#where,
        Some(ScalarExpr::Func { ref name, .. }) if name == "text_match"
    ));
    assert!(matches!(
        block.access,
        AccessPathPlan::OperatorTree {
            score_limit_pushdown: true
        }
    ));
}

#[test]
fn selects_hybrid_access_and_prioritizes_retrieval_candidates() {
    let UnifiedPlan::Query(query) = optimized(
        "SELECT id FROM docs \
         WHERE id + 1 > 2 AND text_match(body, 'rust')",
    ) else {
        panic!("query plan expected");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block expected");
    };
    assert!(matches!(block.access, AccessPathPlan::Hybrid));
    let Some(ScalarExpr::And(parts)) = block.r#where.as_ref() else {
        panic!("conjunctive predicate expected");
    };
    assert!(matches!(
        parts.first(),
        Some(ScalarExpr::Func { name, .. }) if name == "text_match"
    ));
}

#[test]
fn dpccp_reorders_inner_join_source_from_relation_statistics() {
    let plan = optimized_with_rows(
        "SELECT a.id FROM a \
         JOIN b ON a.id = b.a_id \
         JOIN c ON b.id = c.b_id",
        &[("a", 1_000_000), ("b", 10_000), ("c", 10)],
    );
    let source = query_block(&plan).from.as_ref().expect("join source");
    let SourcePlan::Join {
        left, right, on, ..
    } = source
    else {
        panic!("top-level join expected");
    };

    let left_aliases = source_aliases(left);
    let right_aliases = source_aliases(right);
    let small_pair = BTreeSet::from(["b".to_string(), "c".to_string()]);
    assert!(
        left_aliases == small_pair || right_aliases == small_pair,
        "unexpected DPccp source: {source:?}"
    );
    assert!(on.is_some(), "a-b predicate must remain on the root join");
    assert_eq!(source_predicate_count(source), 2);
}

#[test]
fn join_reordering_preserves_outer_join_boundary() {
    let plan = optimized_with_rows(
        "SELECT a.id FROM a \
         LEFT JOIN b ON a.id = b.a_id \
         JOIN c ON b.id = c.b_id",
        &[("a", 1_000_000), ("b", 10_000), ("c", 1)],
    );
    let source = query_block(&plan).from.as_ref().expect("join source");
    let SourcePlan::Join {
        left,
        right,
        kind: uqa_sql::ast::JoinKind::Inner,
        lateral: false,
        ..
    } = source
    else {
        panic!("original top-level inner join must remain");
    };
    assert!(matches!(
        left.as_ref(),
        SourcePlan::Join {
            kind: uqa_sql::ast::JoinKind::Left,
            ..
        }
    ));
    assert_eq!(source_aliases(right), BTreeSet::from(["c".to_string()]));
    assert_eq!(source_predicate_count(source), 2);
}

#[test]
fn join_reordering_preserves_lateral_boundary() {
    let mut statements = compile(
        "SELECT a.id FROM a \
         JOIN b ON a.id = b.a_id \
         JOIN c ON b.id = c.b_id",
    )
    .expect("SQL compiles");
    let mut plan = UnifiedPlan::lower(statements.remove(0));
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("query plan expected");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block expected");
    };
    let SourcePlan::Join { lateral, .. } = block.from.as_mut().expect("join source") else {
        panic!("join expected");
    };
    *lateral = true;

    let rows = BTreeMap::from([("a", 1_000_000), ("b", 10_000), ("c", 1)]);
    let plan = optimize_with_statistics(plan, &OptimizerConfig::default(), &|table: &str| {
        rows.get(table).copied().map(|row_count| {
            let column = || crate::ColumnStats {
                distinct_count: row_count,
                row_count,
                ..crate::ColumnStats::default()
            };
            crate::RelationStats::new(row_count)
                .with_column("id", column())
                .with_column("a_id", column())
                .with_column("b_id", column())
        })
    })
    .expect("optimizer succeeds");
    let source = query_block(&plan).from.as_ref().expect("join source");
    let SourcePlan::Join {
        left,
        right,
        lateral: true,
        strategy: JoinExecutionStrategy::Auto,
        ..
    } = source
    else {
        panic!("lateral root boundary must remain unchanged: {source:?}");
    };
    assert_eq!(
        source_aliases(left),
        BTreeSet::from(["a".to_string(), "b".to_string()])
    );
    assert_eq!(source_aliases(right), BTreeSet::from(["c".to_string()]));
    assert!(matches!(
        left.as_ref(),
        SourcePlan::Join {
            strategy: JoinExecutionStrategy::Hash,
            lateral: false,
            ..
        }
    ));
}
