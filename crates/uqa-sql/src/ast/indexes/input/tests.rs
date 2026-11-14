//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Expr, Statement};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

mod compatibility;
mod retention;

type Decoder = ImageDecoder<BudgetedVec<Budgeted<String>>, JsonInputError>;

fn input(text: Arc<str>, memory: &MemoryBudget, token: &CancellationToken) -> Decoder {
    ImageDecoder::new(memory, token, |mut reader| async move {
        IndexKey::read_stored_columns_from(&text, &mut reader.input()).await
    })
    .unwrap()
}

fn finish(
    input: &mut Decoder,
    steps: usize,
) -> Result<BudgetedVec<Budgeted<String>>, JsonInputError> {
    for _ in 0..1_000_000 {
        if let Some(value) =
            input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok(value);
        }
    }
    panic!("stored index key input stopped progressing")
}

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<Vec<IndexKey>>(text);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let mut input = input(text.into(), &memory, &token);
    let actual = finish(&mut input, 1);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: actual={actual:?}, expected={expected:?}"
    );
    let valid = expected.is_ok();
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => {
            let expected = expected
                .iter()
                .filter_map(IndexKey::column)
                .collect::<Vec<_>>();
            assert_eq!(
                actual.iter().map(|name| name.as_str()).collect::<Vec<_>>(),
                expected,
                "{text}"
            );
            assert_eq!(
                memory.used(),
                actual.capacity() * size_of::<Budgeted<String>>()
                    + actual.iter().map(|name| name.capacity()).sum::<usize>()
            );
        }
        (Err(JsonInputError::Diagnostic(actual)), Err(expected)) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.position().line, expected.line(), "{text}");
            assert_eq!(actual.position().column, expected.column(), "{text}");
            assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text}");
            assert_eq!(actual.is_eof(), expected.is_eof(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data(), "{text}");
        }
        (actual, expected) => panic!("{text}: actual={actual:?}, expected={expected:?}"),
    }
    assert!(input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .is_err());
    drop(input);
    assert_eq!(memory.used(), 0);
    valid
}

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

fn query(sql: &str) -> serde_json::Value {
    let Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("query fixture: {sql}")
    };
    serde_json::to_value(Expr::ScalarSubquery(query)).unwrap()
}

fn keys(expression: serde_json::Value) -> String {
    serde_json::to_string(
        &serde_json::json!(["body", expression, {"Column":"expression_column"}, "Star", "body"]),
    )
    .unwrap()
}

#[test]
fn stored_index_columns_preserve_complete_queries_windows_and_command_ctes() {
    for sql in QUERIES {
        assert!(compare(&keys(query(sql))), "{sql}");
    }
    for expression in [
        r#"{"WindowCall":{"name":"count","args":[],"spec":{"partition_by":[],"order_by":[],"frame":{"mode":"Groups","start":{"Preceding":{"Literal":1}},"end":"CurrentRow","exclusion":"Ties"}},"modifiers":{"distinct":true,"ordered":true,"within_group":false}}}"#,
        r#"{"Func":{"name":"bound","binding":{"name":"selected","argument_types":["text"],"dispatch":{"TypeOf":{"type_oid":23}}},"args":[{"WindowCall":{"name":"f","args":[],"spec":{"partition_by":[],"order_by":[]}}}],"distinct":false,"order_by":[],"filter":null}}"#,
        r#"{"TypedLiteral":{"value":{"kept":[1,2,"a"]},"ty":"record","collation_oid":100,"composite_source":{"value":{"original":"retained"},"descriptors":[{"dropped":[{"number":2,"length":-1,"alignment":100}],"type_oid":40000,"relation_oid":50000,"attributes":[{"name":"a","ty":{"Array":"Text"},"collation_oid":100,"number":1}]}]}}}"#,
    ] {
        assert!(
            compare(&format!("[\"column\",{expression}]")),
            "{expression}"
        );
    }
}

fn mutate(value: &serde_json::Value, visit: &mut dyn FnMut(serde_json::Value)) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, original) in fields {
                let mut changed = fields.clone();
                changed.remove(name);
                visit(changed.into());
                for replacement in [
                    serde_json::Value::Null,
                    serde_json::json!(false),
                    serde_json::json!(-0.0),
                    serde_json::json!([]),
                    serde_json::json!({}),
                ] {
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
        serde_json::Value::Array(items) => {
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
fn stored_index_columns_match_original_field_defaults_types_and_nested_rejections() {
    for sql in QUERIES {
        let original = query(sql);
        mutate(&original, &mut |changed| {
            compare(&keys(changed));
        });
    }
    for text in [
        "null",
        "{}",
        "[null]",
        "[false]",
        "[0]",
        r#"[{"Unknown":null}]"#,
        r#"["ok",{"RetainedLiteral":null}]"#,
        r#"[{"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[],"frame":null},"modifiers":null}}]"#,
    ] {
        assert!(!compare(text));
    }
    let text = keys(query("SELECT 1"));
    for end in 0..text.len() {
        compare(&text[..end]);
    }
}

#[test]
fn stored_index_columns_retain_only_names_and_release_interrupted_validation() {
    let expression = serde_json::json!({"WindowCall":{"name":"x","args":[],"spec":{"partition_by":[],"order_by":[],"unknown":"x".repeat(1 << 20)}}});
    let text: Arc<str> = keys(expression).into();
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let mut read = input(text, &memory, &token);
    let columns = finish(&mut read, 1).unwrap();
    assert_eq!(
        columns.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["body", "Star", "body"]
    );
    drop((read, columns));
    assert_eq!(memory.used(), 0);
    for outcome in ["cancel", "memory", "zero", "drop", "unwind"] {
        let text: Arc<str> = format!("[\"first\",\"{}\"]", "large".repeat(1 << 16)).into();
        let memory = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let mut read = input(text, &memory, &token);
        for _ in 0..8 {
            assert!(read
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
        }
        let occupied = (outcome == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if outcome == "cancel" {
            token.cancel();
        }
        if outcome == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _read = read;
                    panic!("abandoned stored index keys");
                }))
                .is_err()
            );
        } else {
            if outcome != "drop" {
                assert!(finish(&mut read, usize::from(outcome != "zero")).is_err());
            }
            drop(read);
        }
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}
