//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_execution::ScalarExpr;
use uqa_sql::compile;

use super::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

fn one(sql: &str) -> UnifiedPlan {
    let mut statements = compile(sql).expect("SQL compiles");
    assert_eq!(statements.len(), 1);
    UnifiedPlan::lower(statements.remove(0))
}

#[test]
fn alter_routine_lowers_as_an_exact_identity_command() {
    let plan = one("ALTER FUNCTION app.f(integer, text) IMMUTABLE STRICT");
    assert_eq!(plan.name(), "AlterRoutine");
    let UnifiedPlan::Command(command) = plan else {
        panic!("ALTER FUNCTION must be a command plan");
    };
    let CommandPlan::AlterRoutine(alter) = command.as_ref() else {
        panic!("expected ALTER routine command");
    };
    assert_eq!(alter.kind, uqa_sql::ast::AlterRoutineKind::Function);
    assert_eq!(alter.name, "app.f");
    assert_eq!(alter.arg_types.as_deref().unwrap(), ["int4", "text"]);
    assert!(alter.arg_type_references.is_empty());
    assert_eq!(
        alter.volatility,
        Some(uqa_sql::ast::FunctionVolatility::Immutable)
    );
    assert_eq!(alter.strict, Some(true));
}

#[test]
fn arithmetic_and_window_are_relational_compute_nodes() {
    let arithmetic = one("SELECT a + 1 AS b FROM t");
    let UnifiedPlan::Query(query) = arithmetic else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    assert!(matches!(block.compute, ComputePlan::Project));

    let window = one("SELECT row_number() OVER (ORDER BY a) AS n FROM t");
    let UnifiedPlan::Query(query) = window else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    assert!(matches!(block.compute, ComputePlan::Window));
}

#[test]
fn from_and_scalar_subqueries_own_query_children() {
    let plan = one("SELECT (SELECT max(x) FROM inner_t) AS m FROM (SELECT y FROM outer_t) AS s");
    let UnifiedPlan::Query(query) = plan else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    assert!(matches!(block.from, Some(SourcePlan::Subquery { .. })));
    assert_eq!(block.subqueries.len(), 1);
}

#[test]
fn range_function_groups_lower_each_member_with_an_independent_binding_slot() {
    let UnifiedPlan::Query(query) = one(
        "SELECT * FROM ROWS FROM (f(1) AS (left_id int4, left_label text), g(2)) \
         WITH ORDINALITY AS grouped(a, b, c, sequence)",
    ) else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    let Some(SourcePlan::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ordinality,
    }) = &block.from
    else {
        panic!("expected function group");
    };
    assert_eq!(alias.as_deref(), Some("grouped"));
    assert_eq!(column_aliases.as_slice(), ["a", "b", "c", "sequence"]);
    assert!(*ordinality);
    assert_eq!(functions.len(), 2);
    assert_eq!(functions[0].name, "f");
    assert!(functions[0].binding.is_none());
    assert_eq!(functions[0].args.len(), 1);
    assert_eq!(
        functions[0].column_aliases.as_slice(),
        ["left_id", "left_label"]
    );
    assert_eq!(functions[0].column_types.as_slice(), ["int4", "text"]);
    assert_eq!(functions[1].name, "g");
    assert!(functions[1].binding.is_none());
    assert_eq!(functions[1].args.len(), 1);
    assert_eq!(block.subqueries.len(), 0);
}

#[test]
fn multi_argument_unnest_lowers_as_a_canonical_function_group() {
    let UnifiedPlan::Query(query) = one("SELECT * FROM unnest(ARRAY[1], ARRAY[2])") else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    let source = block.from.as_ref().expect("FROM source");
    assert_eq!(source.visible_qualifier(), Some("unnest"));
    let SourcePlan::FunctionGroup { functions, .. } = source else {
        panic!("expected function group");
    };
    assert_eq!(functions.len(), 2);
    assert!(functions.iter().all(|function| {
        function.name == "pg_catalog.unnest"
            && function.output_name == "unnest"
            && function.binding.is_none()
            && function.args.len() == 1
    }));
}

#[test]
fn set_operations_and_ctes_are_structural_children() {
    let plan = one("WITH q AS (SELECT 1 AS x) SELECT x FROM q UNION SELECT 2");
    let UnifiedPlan::Query(query) = plan else {
        panic!("expected query plan");
    };
    assert_eq!(query.ctes.len(), 1);
    assert!(matches!(query.root, RelationalPlan::SetOp { .. }));
}

#[test]
fn fetch_with_ties_survives_query_block_and_set_operation_lowering() {
    let UnifiedPlan::Query(query) = one("SELECT x FROM t ORDER BY x FETCH FIRST 2 ROWS WITH TIES")
    else {
        panic!("expected query plan");
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("expected query block");
    };
    assert!(block.with_ties);

    let UnifiedPlan::Query(query) = one(
        "SELECT x FROM left_t UNION ALL SELECT x FROM right_t ORDER BY x FETCH FIRST 2 ROWS WITH TIES",
    ) else {
        panic!("expected set-operation query plan");
    };
    assert!(matches!(
        query.root,
        RelationalPlan::SetOp {
            with_ties: true,
            ..
        }
    ));
}

#[test]
fn values_is_a_query_plan_not_a_command_escape_hatch() {
    let plan = one("VALUES (1 + 2), (3 + 4)");
    let UnifiedPlan::Query(query) = plan else {
        panic!("VALUES must be relational");
    };
    assert!(matches!(query.root, RelationalPlan::Values { .. }));
}

#[test]
fn mutations_own_source_and_scalar_query_children() {
    let update = one("WITH limits AS (SELECT max(v) AS v FROM source) \
         UPDATE target SET v = (SELECT v FROM limits) FROM source \
         WHERE target.id = source.id");
    let UnifiedPlan::Command(update) = update else {
        panic!("UPDATE must be a command plan");
    };
    let CommandPlan::Update(update) = update.as_ref() else {
        panic!("expected UPDATE plan");
    };
    assert_eq!(update.ctes.len(), 1);
    assert!(matches!(
        update.source.as_deref(),
        Some(SourcePlan::Table { .. })
    ));
    assert_eq!(update.subqueries.len(), 1);

    let merge = one("MERGE INTO target USING (SELECT id, v FROM source) AS s \
         ON target.id = s.id WHEN MATCHED THEN UPDATE SET v = s.v");
    let UnifiedPlan::Command(merge) = merge else {
        panic!("MERGE must be a command plan");
    };
    let CommandPlan::Merge(merge) = merge.as_ref() else {
        panic!("expected MERGE plan");
    };
    assert!(matches!(merge.source.as_ref(), SourcePlan::Subquery { .. }));
}

#[test]
fn scalar_rewriter_reaches_ctes_subqueries_and_relational_slots() {
    let mut plan = one("WITH q AS (SELECT arg AS x) \
         SELECT arg + (SELECT arg) FROM q \
         WHERE arg > 0 ORDER BY arg LIMIT arg");
    plan.rewrite_scalar_expressions(&mut |expression| {
        if matches!(expression, ScalarExpr::Column(name) if name == "arg") {
            *expression = ScalarExpr::Param(1);
        }
    });

    let mut named = 0;
    let mut parameters = 0;
    plan.rewrite_scalar_expressions(&mut |expression| match expression {
        ScalarExpr::Column(name) if name == "arg" => named += 1,
        ScalarExpr::Param(1) => parameters += 1,
        _ => {}
    });
    assert_eq!(named, 0);
    assert!(parameters >= 6, "all nested scalar slots must be visited");
}

#[test]
fn query_scalar_rewriter_visits_every_node_once() {
    let UnifiedPlan::Query(mut query) = one("SELECT x + 5 FROM (VALUES (1 + 2)) AS v(x) \
         WHERE x + 3 > 0 ORDER BY x + 4 LIMIT 6 + 7")
    else {
        panic!("expected query plan");
    };
    let mut visits = std::collections::BTreeMap::<usize, usize>::new();
    query.rewrite_scalar_expressions(&mut |expression| {
        *visits
            .entry(std::ptr::from_mut::<ScalarExpr>(expression) as usize)
            .or_default() += 1;
    });

    // Five expression roots own 17 nodes in total: the VALUES source,
    // projection, predicate, ordering, and limit. Pointer identity proves
    // that recursive traversal did not invoke the callback twice for any
    // node (including a binary lhs or VALUES cell).
    assert_eq!(visits.len(), 17);
    assert!(visits.values().all(|visits| *visits == 1), "{visits:?}");
}
