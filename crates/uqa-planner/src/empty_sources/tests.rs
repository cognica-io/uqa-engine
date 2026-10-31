//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn query(sql: &str) -> Box<QueryPlan> {
    let plan = crate::optimize(
        crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0)),
        &crate::OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
    )
    .unwrap();
    let (plan, memory) = plan.into_parts();
    assert_eq!(memory.reserved_bytes(), 0);
    let crate::UnifiedPlan::Query(query) = plan else {
        panic!("query")
    };
    query
}

#[test]
fn empty_source_proofs_preserve_scan_initialization_and_implicit_groups() {
    for (sql, empty) in [
        ("SELECT v FROM input WHERE false", true),
        ("SELECT v FROM input WHERE 1 = 0", true),
        ("SELECT v FROM input WHERE NULL::boolean", true),
        ("SELECT v FROM input", false),
        ("SELECT v FROM input WHERE false OFFSET -1", false),
        ("SELECT nextval('s') FROM input WHERE false", false),
        ("SELECT 1 WHERE false UNION ALL SELECT 2 WHERE false", false),
        ("SELECT v FROM input LIMIT 0", false),
        ("SELECT v FROM input WHERE v = 0", false),
        ("SELECT count(*) FROM input WHERE false", false),
        ("SELECT v FROM input WHERE false GROUP BY v", true),
        (
            "SELECT count(*) FROM input WHERE false GROUP BY GROUPING SETS ((), (v))",
            false,
        ),
        (
            "WITH c AS MATERIALIZED (SELECT 1) SELECT v FROM input WHERE false",
            false,
        ),
    ] {
        assert_eq!(query_is_empty(&query(sql)), empty, "{sql}");
    }
}

#[test]
fn empty_join_proofs_preserve_both_outer_join_sides() {
    for (kind, left, right, expected, empty_inputs) in [
        (
            "CROSS JOIN",
            "SELECT 1 AS v WHERE false",
            "SELECT 2 AS w",
            true,
            (true, false),
        ),
        (
            "LEFT JOIN",
            "SELECT 1 AS v WHERE false",
            "SELECT 2 AS w",
            true,
            (true, true),
        ),
        (
            "LEFT JOIN",
            "SELECT 1 AS v",
            "SELECT 2 AS w",
            false,
            (false, true),
        ),
        (
            "RIGHT JOIN",
            "SELECT 1 AS v",
            "SELECT 2 AS w",
            false,
            (true, false),
        ),
        (
            "FULL JOIN",
            "SELECT 1 AS v",
            "SELECT 2 AS w",
            false,
            (false, false),
        ),
        (
            "FULL JOIN",
            "SELECT 1 AS v WHERE false",
            "SELECT 2 AS w WHERE false",
            true,
            (true, true),
        ),
    ] {
        let on = if kind == "CROSS JOIN" {
            ""
        } else {
            " ON false"
        };
        let sql = format!("SELECT * FROM ({left}) l {kind} ({right}) r{on}");
        let plan = query(&sql);
        let RelationalPlan::QueryBlock(block) = &plan.root else {
            panic!("block")
        };
        let source = block.from.as_ref().unwrap();
        assert_eq!(source_is_empty(source), expected, "{sql}");
        assert_eq!(join_input_is_empty(source, false), empty_inputs.0, "{sql}");
        assert_eq!(join_input_is_empty(source, true), empty_inputs.1, "{sql}");
    }
}

#[test]
fn empty_append_requires_relation_inputs_and_identical_analyzed_types() {
    for (operator, expected) in [
        ("UNION ALL", true),
        ("UNION", false),
        ("INTERSECT", false),
        ("EXCEPT", false),
    ] {
        let mut plan = query(&format!(
            "SELECT v FROM input WHERE false {operator} SELECT v FROM input WHERE false"
        ));
        let schema = uqa_sql::RowSchema::with_types(
            vec!["v".into()],
            vec![Some(uqa_sql::ColumnType::Integer)],
        );
        plan.analyzed_output = Some(schema.clone());
        let RelationalPlan::SetOp { left, right, .. } = &mut plan.root else {
            panic!("set operator")
        };
        left.analyzed_output = Some(schema.clone());
        right.analyzed_output = Some(schema);
        assert_eq!(query_is_empty(&plan), expected, "{operator}");
        let RelationalPlan::SetOp { right, .. } = &mut plan.root else {
            unreachable!()
        };
        right.analyzed_output = Some(uqa_sql::RowSchema::with_types(
            vec!["v".into()],
            vec![Some(uqa_sql::ColumnType::BigInteger)],
        ));
        assert!(!query_is_empty(&plan), "coercing {operator}");
    }
}
