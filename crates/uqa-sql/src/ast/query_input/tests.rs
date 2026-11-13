//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::Statement;
use serde_json::{json, Value};
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod alternatives;
mod compatibility;
mod resources;

const QUERIES: &[&str] = &[
    "SELECT t.a AS named, (SELECT 1) FROM t LEFT JOIN u USING (a) WHERE t.a > 0 ORDER BY named LIMIT 3 OFFSET 1 FOR UPDATE OF t",
    "SELECT sum(a) OVER w FROM t WINDOW w AS (PARTITION BY b ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
    "SELECT a, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((a), ()) HAVING count(*) > 0",
    "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 4",
    "WITH q AS (VALUES (1, 'a'), (2, 'b')) SELECT * FROM q",
    "SELECT * FROM LATERAL (VALUES (1)) AS v(a), LATERAL (SELECT 2) AS q(b)",
    "SELECT * FROM ROWS FROM (generate_series(1, 3), unnest(ARRAY[1, 2])) WITH ORDINALITY AS q(a, b, n)",
    "WITH RECURSIVE q(a) AS (VALUES (1) UNION ALL SELECT a + 1 FROM q WHERE a < 3) SEARCH DEPTH FIRST BY a SET path CYCLE a SET cycled TO true DEFAULT false USING trail SELECT * FROM q",
    "WITH q AS (INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a) SELECT * FROM q",
    "WITH q AS (UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
    "WITH q AS (DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a) SELECT * FROM q",
    "WITH q AS (MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a) SELECT * FROM q",
    "SELECT (SELECT sum(a) OVER () FROM t) + 1 WHERE EXISTS (SELECT 1) AND 1 IN (SELECT 1)",
    "SELECT array_agg((SELECT 1) ORDER BY (SELECT 2) DESC NULLS LAST) FILTER (WHERE EXISTS (SELECT 3))",
];

fn query(sql: &str) -> Value {
    let Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("query fixture: {sql}")
    };
    serde_json::to_value(Expr::ScalarSubquery(query)).unwrap()
}

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<Expr>(text);
    let memory = MemoryBudget::new(64 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let actual = Expr::from_stored_query_json(text, &memory, &original, &invoking);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?} / {expected:?}"
    );
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
        assert!(actual.reserved_bytes() <= memory.used());
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

#[test]
fn stored_query_input_constructs_queries_windows_and_all_mutation_ctes() {
    for sql in QUERIES {
        let encoded = query(sql).to_string();
        assert!(compare(&encoded), "{sql}");
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        assert!(Expr::from_stored_schema_json(&encoded, &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

fn mutate(value: &Value, visit: &mut dyn FnMut(Value)) {
    match value {
        Value::Object(fields) => {
            for (name, original) in fields {
                let mut changed = fields.clone();
                changed.remove(name);
                visit(changed.into());
                for replacement in [Value::Null, json!(false), json!(-0.0), json!([]), json!({})] {
                    let mut changed = fields.clone();
                    changed.insert(name.clone(), replacement);
                    visit(changed.into());
                }
                mutate(original, &mut |replacement| {
                    let mut changed = fields.clone();
                    changed.insert(name.clone(), replacement);
                    visit(changed.into());
                });
            }
        }
        Value::Array(items) => {
            for (index, original) in items.iter().enumerate() {
                mutate(original, &mut |replacement| {
                    let mut changed = items.clone();
                    changed[index] = replacement;
                    visit(changed.into());
                });
            }
        }
        _ => (),
    }
}

#[test]
fn stored_query_input_matches_original_field_omissions_and_wrong_types() {
    for sql in QUERIES {
        mutate(&query(sql), &mut |value| {
            compare(&value.to_string());
        });
    }
}
