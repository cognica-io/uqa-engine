//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn score_descriptor_follows_cte_definition_scope_and_demand() {
    let scope = crate::catalog::test_support::empty_scope();
    for (sql, required) in [
        (
            "SELECT id FROM docs WHERE text_match(body, 'rust')",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')), \
             retained AS (SELECT * FROM hits) \
             SELECT * FROM (WITH hits AS (VALUES (1)) SELECT DISTINCT * FROM retained) AS output",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) \
             SELECT * FROM (WITH hits AS (VALUES (1)) SELECT DISTINCT * FROM hits) AS output",
            false,
        ),
        (
            "WITH unused AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT 1",
            false,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT * FROM (WITH hits AS (SELECT * FROM hits) SELECT * FROM hits) s",
            true,
        ),
        (
            "WITH hits AS (SELECT id FROM docs WHERE text_match(body, 'rust')) SELECT * FROM (WITH retained AS (SELECT * FROM hits), hits AS (VALUES (1)) SELECT * FROM retained) s",
            true,
        ),
        (
            "WITH RECURSIVE numbers(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM numbers WHERE n < 3) SELECT * FROM numbers",
            false,
        ),
    ] {
        let uqa_sql::plan::UnifiedPlan::Query(query) =
            uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("expected query");
        };
        assert_eq!(requires_physical_descriptor(&query, &scope).unwrap(), required, "{sql}");
    }
}
