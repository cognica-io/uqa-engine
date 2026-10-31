//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{QueryPlan, RelationalPlan};
use crate::{RowSchema, ScalarExpr};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn plan_observation_counts_actual_payload_and_capacity_without_copying() {
    let mut text = String::with_capacity(4 * 1024 * 1024);
    text.push_str("payload");
    let text_capacity = text.capacity();
    let mut row = Vec::with_capacity(7);
    row.push(ScalarExpr::Literal(Value::Str(text)));
    let mut rows = Vec::with_capacity(3);
    rows.push(row);
    let expected_used =
        size_of::<QueryPlan>() + size_of::<Vec<ScalarExpr>>() + size_of::<ScalarExpr>() + 7;
    let expected_allocated = size_of::<QueryPlan>()
        + rows.capacity() * size_of::<Vec<ScalarExpr>>()
        + rows[0].capacity() * size_of::<ScalarExpr>()
        + text_capacity;
    let plan = UnifiedPlan::Query(Box::new(QueryPlan {
        analyzed_output: Some(RowSchema::with_types(
            vec!["external descriptor".repeat(1000)],
            vec![Some(crate::ColumnType::Text)],
        )),
        analyzed_source: None,
        analyzed_grouping: None,
        relations_bound: false,
        relation_privilege_subject: None,
        ctes: Vec::new(),
        root: RelationalPlan::Values {
            rows,
            subqueries: Vec::new(),
        },
    }));
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocations = allocation_counter::measure(|| {
        let measured = plan.planning_memory(&control).unwrap();
        assert_eq!(measured.used_bytes, expected_used);
        assert_eq!(measured.allocated_bytes, expected_allocated);
    });
    assert!(allocations.bytes_max < 4096, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn executable_observation_covers_queries_and_mutations_without_changing_the_ir() {
    for sql in [
        "SELECT t.a AS named, (SELECT 1) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY named LIMIT 3 OFFSET 1 FOR UPDATE OF t",
        "SELECT sum(a) OVER w FROM t WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
        "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
        "WITH d AS (DELETE FROM t RETURNING a) INSERT INTO u SELECT a FROM d RETURNING a",
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN UPDATE SET b = u.b WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let before = serde_json::to_value(&plan).unwrap();
        let measured = plan.planning_memory(&ProductionControl::uncontrolled()).unwrap();
        assert!(measured.used_bytes > 0 && measured.used_bytes <= measured.allocated_bytes, "{sql}");
        assert_eq!(serde_json::to_value(&plan).unwrap(), before, "{sql}");
    }
}
