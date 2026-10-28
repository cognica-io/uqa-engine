//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
            let result = reordered_inner_join_source(source, &[], &|_: &str| None).unwrap();
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
        let allocation = allocation_counter::measure(|| {
            reorder_unified_plan_joins(&mut plan, &|_: &str| None).unwrap();
        });
        assert!(
            allocation.bytes_total < 4096,
            "where={where_clause}: {allocation:?}"
        );
        assert_eq!(allocation.bytes_current, 0);
    }
}
