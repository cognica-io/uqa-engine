//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn recomputed_aggregate_kind_preserves_qualified_builtin_and_selected_scalar() {
    for sql in [
        "SELECT pg_catalog.count(*) FROM t",
        "SELECT pg_catalog.sum(n) FROM t",
        "SELECT coalesce(pg_catalog.count(*), 0) FROM t",
    ] {
        assert!(
            matches!(query_block(&optimized(sql)).compute, ComputePlan::Aggregate),
            "{sql}"
        );
    }
    let mut plan = UnifiedPlan::lower(compile("SELECT count() FROM t").unwrap().remove(0));
    let UnifiedPlan::Query(query) = &mut plan else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let ScalarExpr::Func { binding, .. } = &mut block.projections[0].expr else {
        unreachable!()
    };
    let mut selected = uqa_sql::ast::FunctionBinding::builtin_by_name("app.count", None);
    selected.builtin = false;
    selected.object_id = Some([42; 16]);
    *binding = Some(selected);
    let plan = optimize(
        plan,
        &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar),
    )
    .unwrap();
    assert!(matches!(query_block(&plan).compute, ComputePlan::Project));
}
