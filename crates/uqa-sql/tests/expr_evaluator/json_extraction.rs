//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{eval, projection_expr, where_expr, EvalContext, Value};
use uqa_sql::{ast::ColumnType, plan::ExpressionPlan, type_resolution::scalar_type, RowSchema};

#[test]
fn jsonb_extraction_comparison_preserves_static_type() {
    let schema = RowSchema::with_types(vec!["basis".into()], vec![Some(ColumnType::Text)]);
    for sql in [
        "SELECT * FROM probe WHERE basis::jsonb->'query'='{}'::jsonb",
        "SELECT * FROM probe WHERE basis::jsonb#>'{query}'='{}'::jsonb",
    ] {
        let expression = ExpressionPlan::lower(where_expr(sql));
        assert_eq!(
            scalar_type(&expression.scalar, &schema, &[]).unwrap(),
            Some(ColumnType::Boolean),
            "{sql}"
        );
    }
}

#[test]
fn json_extraction_result_types_follow_input_and_operator() {
    for (sql, expected) in [
        ("SELECT '{}'::jsonb->'query'", ColumnType::JsonB),
        ("SELECT '{}'::json->'query'", ColumnType::Json),
        ("SELECT '{}'::jsonb#>'{query}'", ColumnType::JsonB),
        ("SELECT '{}'::json#>'{query}'", ColumnType::Json),
        ("SELECT '[1]'::jsonb->0", ColumnType::JsonB),
        ("SELECT '{}'::jsonb->>'query'", ColumnType::Text),
        ("SELECT '{}'::jsonb#>>'{query}'", ColumnType::Text),
        (
            "SELECT json_extract_path('{}'::json, 'query')",
            ColumnType::Json,
        ),
    ] {
        let expression = ExpressionPlan::lower(projection_expr(sql));
        assert_eq!(
            scalar_type(&expression.scalar, &RowSchema::default(), &[]).unwrap(),
            Some(expected),
            "{sql}"
        );
    }
    let expression =
        ExpressionPlan::lower(projection_expr("SELECT ('{}'::json->'query') = '{}'::json"));
    assert_eq!(
        scalar_type(&expression.scalar, &RowSchema::default(), &[])
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
}

#[test]
fn json_extraction_distinguishes_json_null_and_missing() {
    for (sql, expected) in [
        (
            r#"SELECT '{"query":null}'::jsonb->'query'"#,
            Value::JsonB("null".into()),
        ),
        (
            r#"SELECT '{"query":null}'::json->'query'"#,
            Value::Json("null".into()),
        ),
        (
            r#"SELECT '{"query":null}'::jsonb#>'{query}'"#,
            Value::JsonB("null".into()),
        ),
        ("SELECT '{}'::jsonb->'query'", Value::Null),
        (r#"SELECT '{"query":null}'::jsonb->>'query'"#, Value::Null),
        ("SELECT NULL::jsonb->'query'", Value::Null),
        ("SELECT '{}'::jsonb->NULL::text", Value::Null),
        (
            r#"SELECT '{"a,b":null}'::jsonb#>ARRAY['a,b']"#,
            Value::JsonB("null".into()),
        ),
        ("SELECT '{}'::jsonb#>'{}'", Value::JsonB("{}".into())),
        ("SELECT '{}'::jsonb#>ARRAY[NULL::text]", Value::Null),
    ] {
        assert_eq!(
            eval(&projection_expr(sql), &EvalContext::new(None, &[])).unwrap(),
            expected,
            "{sql}"
        );
    }
}

#[test]
fn json_extraction_operators_keep_key_and_index_overloads() {
    for (sql, expected) in [
        (r#"SELECT '{"0":1}'::jsonb -> 0"#, Value::Null),
        (
            r#"SELECT '{"0":1}'::jsonb -> '0'"#,
            Value::JsonB("1".into()),
        ),
        ("SELECT '[1]'::jsonb -> '0'", Value::Null),
        ("SELECT '[1]'::jsonb -> 0", Value::JsonB("1".into())),
        ("SELECT '[1]'::jsonb #> '{0}'", Value::JsonB("1".into())),
        (
            r#"SELECT '{"0":1}'::jsonb #> '{0}'"#,
            Value::JsonB("1".into()),
        ),
    ] {
        assert_eq!(
            eval(&projection_expr(sql), &EvalContext::new(None, &[])).unwrap(),
            expected,
            "{sql}"
        );
    }
    let expression = ExpressionPlan::lower(projection_expr("SELECT '{}'::text -> 'a'"));
    assert_eq!(
        scalar_type(&expression.scalar, &RowSchema::default(), &[])
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
}

#[test]
fn json_extraction_sql_rendering_round_trips() {
    for sql in [
        "SELECT '{}'::jsonb -> 'a'",
        "SELECT '[1]'::jsonb ->> 0",
        "SELECT '{}'::jsonb #> '{a,b}'",
        "SELECT '{}'::jsonb #>> '{a,b}'",
        r#"SELECT '{"a,b":1}'::jsonb #> ARRAY['a,b']"#,
    ] {
        let original = projection_expr(sql);
        let rendered = uqa_sql::render::expression_sql(&original).unwrap();
        let reparsed = projection_expr(&format!("SELECT {rendered}"));
        assert_eq!(
            eval(&original, &EvalContext::new(None, &[])).unwrap(),
            eval(&reparsed, &EvalContext::new(None, &[])).unwrap(),
            "{sql}: {rendered}"
        );
        assert_eq!(
            scalar_type(
                &ExpressionPlan::lower(original).scalar,
                &RowSchema::default(),
                &[]
            )
            .unwrap(),
            scalar_type(
                &ExpressionPlan::lower(reparsed).scalar,
                &RowSchema::default(),
                &[]
            )
            .unwrap()
        );
    }
}
