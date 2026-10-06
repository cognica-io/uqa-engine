//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{EnumLabelKey, EnumValue, Value};

use super::{expression_sql, statement_sql};
use crate::ast::{BinaryOp, Expr, FromClause, FunctionOrderSyntax, Statement};
use crate::SQLError;

#[test]
fn named_window_rendering_preserves_shared_inputs_and_nested_scopes() {
    for sql in [
        "SELECT row_number() OVER w + row_number() OVER (w) AS n WINDOW w AS (ORDER BY '{1}'::integer[]), unused AS (PARTITION BY 3)",
        "SELECT DISTINCT ON (row_number() OVER w) row_number() OVER (w) AS n WINDOW w AS (ORDER BY '{1}'::integer[])",
        "SELECT sum(v) OVER \"Framed W\" FROM t WINDOW base AS (PARTITION BY g), ordered AS (base ORDER BY v), \"Framed W\" AS (ordered ROWS UNBOUNDED PRECEDING) ORDER BY row_number() OVER ordered",
        "WITH q AS (SELECT row_number() OVER w AS n WINDOW w AS (ORDER BY 1)) SELECT row_number() OVER w FROM q WINDOW w AS (ORDER BY n DESC)",
    ] {
        let statement = crate::compile(sql).unwrap().remove(0);
        let rendered = statement_sql(&statement).unwrap();
        let reparsed = crate::compile(&rendered).unwrap().remove(0);
        assert_eq!(statement_sql(&reparsed).unwrap(), rendered, "{sql}");
        assert!(rendered.contains(" WINDOW "), "{rendered}");
        if sql.contains("'{1}'") {
            assert_eq!(rendered.matches("'{1}'").count(), 1, "{rendered}");
            assert!(rendered.contains("OVER w"), "{rendered}");
            assert!(rendered.contains("OVER (w)"), "{rendered}");
        }
    }
}

#[test]
fn standalone_window_expression_keeps_expanded_fallback_without_query_scope() {
    let Statement::Select(select) =
        crate::compile("SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)")
            .unwrap()
            .remove(0)
    else {
        panic!("expected SELECT")
    };
    assert_eq!(
        expression_sql(&select.projections[0].expr).unwrap(),
        "row_number() OVER (ORDER BY 1)"
    );
}

#[test]
fn unrenderable_nested_nodes_return_errors_instead_of_panicking() {
    let value = EnumValue::new(16_384, EnumLabelKey::from_bytes(vec![0x80]).unwrap());
    let expected = crate::expr::catalog_output_required(&value).to_string();
    let comparison = Expr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(Expr::Column("mood".into())),
        rhs: Box::new(Expr::Literal(Value::Enum(value))),
    };
    assert_eq!(
        expression_sql(&comparison).unwrap_err().to_string(),
        expected
    );
    let mut statements = crate::compile(
        "SELECT 1; UPDATE t SET a = 1; SELECT * FROM text_similarity_join(t, t.a, u, u.b, 0.5)",
    )
    .unwrap();
    let [Statement::Select(select), Statement::Update(update), Statement::Select(join)] =
        statements.as_mut_slice()
    else {
        panic!("expected SELECT, UPDATE and operator join SELECT");
    };
    select.projections[0].expr = comparison.clone();
    update.assignments[0].1 = comparison;
    let Some(FromClause::Function { args, .. }) = &mut join.from else {
        panic!("expected an operator join source");
    };
    args.clear();
    for statement in &statements[..2] {
        assert_eq!(statement_sql(statement).unwrap_err().to_string(), expected);
    }
    assert!(matches!(
        statement_sql(&statements[2]),
        Err(SQLError::Internal(_))
    ));
}

#[test]
fn rendered_rule_action_shapes_round_trip_stably() {
    for sql in [
        "SELECT source.key_value, row_number() OVER (ORDER BY source.key_value ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS sequence FROM left_table AS source(key_value, payload) JOIN right_table AS other USING (key_value) WHERE source.payload IS NOT NULL ORDER BY sequence LIMIT 2 OFFSET 1",
        "WITH source(value) AS MATERIALIZED (SELECT 1) SELECT value FROM source UNION ALL SELECT 2 ORDER BY value",
        "INSERT INTO target_table AS target(id, value) VALUES (1, 'one') ON CONFLICT (id) DO UPDATE SET value = excluded.value WHERE target.id = 1 RETURNING WITH (OLD AS before, NEW AS after) after.id",
        "UPDATE target_table AS target SET value = source.value FROM source_table AS source(id, value) WHERE target.id = source.id RETURNING target.id",
        "DELETE FROM target_table AS target USING source_table AS source(id) WHERE target.id = source.id RETURNING target.id",
        "NOTIFY rule_channel, 'payload'",
    ] {
        let mut statements = crate::compile(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
        let rendered = statement_sql(&statements.remove(0))
            .unwrap_or_else(|error| panic!("render {sql}: {error}"));
        let mut reparsed = crate::compile(&rendered)
            .unwrap_or_else(|error| panic!("reparse `{rendered}` from `{sql}`: {error}"));
        let rerendered = statement_sql(&reparsed.remove(0))
            .unwrap_or_else(|error| panic!("rerender `{rendered}`: {error}"));
        assert_eq!(rerendered, rendered, "unstable SQL rendering for `{sql}`");
    }
}

#[test]
fn rendered_function_ordering_preserves_written_syntax_and_legacy_output() {
    for sql in [
        "f(a ORDER BY b DESC NULLS LAST) FILTER (WHERE keep)",
        "f(a) WITHIN GROUP (ORDER BY b DESC NULLS LAST) FILTER (WHERE keep)",
        "mode() WITHIN GROUP (ORDER BY b)",
    ] {
        let Statement::Select(mut statement) =
            crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
        else {
            panic!("SELECT expected");
        };
        let expression = statement.projections.remove(0).expr;
        assert_eq!(expression_sql(&expression).unwrap(), sql);
        assert_eq!(
            crate::catalog::expression_text::schema_expr_text(&expression).unwrap(),
            sql
        );
        let Statement::Select(mut reparsed) =
            crate::compile(&format!("SELECT {}", expression_sql(&expression).unwrap()))
                .unwrap()
                .remove(0)
        else {
            panic!("SELECT expected");
        };
        assert_eq!(reparsed.projections.remove(0).expr, expression);
    }
    let Statement::Select(mut statement) =
        crate::compile("SELECT f(a ORDER BY b)").unwrap().remove(0)
    else {
        panic!("SELECT expected");
    };
    let mut expression = statement.projections.remove(0).expr;
    let Expr::Func { order_syntax, .. } = &mut expression else {
        unreachable!();
    };
    *order_syntax = FunctionOrderSyntax::Legacy;
    assert_eq!(expression_sql(&expression).unwrap(), "f(a ORDER BY b)");
    assert_eq!(
        crate::catalog::expression_text::schema_expr_text(&expression).unwrap(),
        "f(a ORDER BY b)"
    );
}
