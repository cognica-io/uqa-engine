//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary output predicates retain scalar, set and recursive evaluation boundaries.

use super::*;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected SELECT")
    };
    *query
}

#[test]
fn predicates_preserve_complete_distinct_groups_and_window_partitions() {
    let fixture = Fixture::new();
    for (sql, allowed) in [
        ("SELECT DISTINCT ON(x) x AS k,10/x AS z FROM (VALUES(0),(2)) v(x) ORDER BY x", true),
        ("SELECT DISTINCT ON(z) x AS k,z FROM (VALUES(0,1),(2,1)) v(x,z) ORDER BY z,x", false),
        ("SELECT x AS k,row_number() OVER (PARTITION BY x) AS n FROM (VALUES(0),(2)) v(x)", true),
        ("SELECT x AS k,row_number() OVER () AS n FROM (VALUES(0),(2)) v(x)", false),
        ("SELECT x AS k,row_number() OVER (PARTITION BY x) AS n,row_number() OVER () AS m FROM (VALUES(0),(2)) v(x)", false),
    ] {
        assert_eq!(push_derived_output_filter(
            fixture.context(), &query(sql), "s", &qualified_literal_equality("s", "k", "2"), None,
        ).unwrap().is_some(), allowed, "{sql}");
    }
}

#[test]
fn ordinary_predicates_select_inputs_before_unrelated_volatile_and_set_outputs() {
    let fixture = Fixture::new();
    let predicate = qualified_literal_equality("s", "k", "2");
    for distinct in ["", "DISTINCT ", "DISTINCT ON(x) "] {
        for output in ["10/x", "nextval('effect')", "unnest(ARRAY[10/x])"] {
            let query = query(&format!(
                "SELECT {distinct}x AS k,{output} AS z FROM (VALUES(0),(2)) v(x)"
            ));
            let specialized =
                push_derived_output_filter(fixture.context(), &query, "s", &predicate, None)
                    .unwrap()
                    .expect("pure predicate on independent output");
            let RelationalPlan::QueryBlock(block) = specialized.root else {
                panic!("expected query block")
            };
            assert_eq!(
                block.r#where,
                Some(ScalarExpr::Binary {
                    op: BinaryOp::Equal,
                    lhs: Box::new(ScalarExpr::Column("x".into())),
                    rhs: Box::new(ScalarExpr::Literal(Value::Str("2".into()))),
                })
            );
            if output.starts_with("nextval") {
                assert!(
                    push_output_filter_into_query_plan(
                        fixture.context(),
                        &query,
                        "s",
                        &predicate,
                        None,
                    )
                    .unwrap()
                    .is_none(),
                    "recursive specialization preserves its volatile boundary"
                );
            }
        }
    }
}

#[test]
fn referenced_effects_and_slicing_keep_the_output_predicate_outside() {
    let fixture = Fixture::new();
    let predicate = qualified_literal_equality("s", "z", "1");
    for sql in [
        "SELECT nextval('effect') AS z",
        "SELECT unnest(ARRAY[1,2]) AS z",
        "SELECT x AS z FROM (VALUES(1),(2)) v(x) LIMIT 1",
        "SELECT x AS z FROM (VALUES(1),(2)) v(x) OFFSET 0",
        "SELECT row_number() OVER () AS z FROM (VALUES(1),(2)) v(x)",
    ] {
        assert!(
            push_derived_output_filter(fixture.context(), &query(sql), "s", &predicate, None,)
                .unwrap()
                .is_none(),
            "{sql}"
        );
    }
}

#[test]
fn output_substitution_preserves_quoted_identity_and_alias_tails() {
    let fixture = Fixture::new();
    let original = query("SELECT 0 AS k,x AS \"K\",10/x AS z FROM (VALUES(0),(2)) v(x)");
    let aliases = ["first".into()];
    let specialized = push_derived_output_filter(
        fixture.context(),
        &original,
        "S",
        &qualified_literal_equality("S", "K", "2"),
        Some(&aliases),
    )
    .unwrap()
    .unwrap();
    let RelationalPlan::QueryBlock(block) = specialized.root else {
        panic!("expected query block")
    };
    let ScalarExpr::Binary { lhs, .. } = block.r#where.unwrap() else {
        panic!("expected comparison")
    };
    assert_eq!(*lhs, ScalarExpr::Column("x".into()));
    assert!(push_derived_output_filter(
        fixture.context(),
        &original,
        "S",
        &qualified_literal_equality("s", "K", "2"),
        None,
    )
    .unwrap()
    .is_none());
}
