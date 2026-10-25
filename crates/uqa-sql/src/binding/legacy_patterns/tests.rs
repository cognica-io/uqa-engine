//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::UnifiedPlan, ColumnType, SQLError};

struct Callbacks(bool);

impl FunctionTypeResolver for Callbacks {
    fn has_untyped_function(&self, _: &str) -> bool {
        self.0
    }

    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

fn expression(sql: &str) -> Expr {
    let crate::Statement::Select(mut query) = crate::compile(&format!("SELECT {sql} AS value"))
        .unwrap()
        .remove(0)
    else {
        panic!("scalar query")
    };
    query.projections.remove(0).expr
}

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile(&format!("SELECT {sql} AS value"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("scalar query")
    };
    *query
}

#[test]
fn predecessor_pattern_markers_restore_nested_helpers_and_negative_identity_once() {
    for (legacy, native) in [
        ("like('a','a')", "'a' LIKE 'a'"),
        ("NOT like('a','b')", "'a' NOT LIKE 'b'"),
        ("ilike('a','A')", "'a' ILIKE 'A'"),
        ("NOT ilike('a','B')", "'a' NOT ILIKE 'B'"),
        ("like('a_b','a!_b','!')", "'a_b' LIKE 'a!_b' ESCAPE '!'"),
        (
            "NOT ilike('a_b','B!_b','!')",
            "'a_b' NOT ILIKE 'B!_b' ESCAPE '!'",
        ),
        ("similar_to('abc','a%')", "'abc' SIMILAR TO 'a%'"),
        (
            "NOT similar_to('a_b','a!_b','!')",
            "'a_b' NOT SIMILAR TO 'a!_b' ESCAPE '!'",
        ),
        ("regex_match_op('a','^a')", "'a' ~ '^a'"),
        ("NOT regex_match_op('a','^b')", "'a' !~ '^b'"),
        ("regex_imatch_op('a','^A')", "'a' ~* '^A'"),
        ("NOT regex_imatch_op('a','^B')", "'a' !~* '^B'"),
        (
            "like(NULL,nextval('s')::text,'!')",
            "NULL LIKE nextval('s')::text ESCAPE '!'",
        ),
    ] {
        let bytes = serde_json::to_vec(&expression(legacy)).unwrap();
        let mut stored: Expr = serde_json::from_slice(&bytes).unwrap();
        assert!(restore_expression(&mut stored, &Callbacks(false)).unwrap());
        assert_eq!(stored, expression(native), "{legacy}");
        let current = serde_json::to_vec(&stored).unwrap();
        assert!(!restore_expression(&mut stored, &Callbacks(false)).unwrap());
        assert_eq!(serde_json::to_vec(&stored).unwrap(), current);

        let mut stored = query(legacy);
        assert!(restore_query(&mut stored, &Callbacks(false)));
        assert_eq!(
            serde_json::to_value(&stored).unwrap(),
            serde_json::to_value(query(native)).unwrap(),
            "{legacy}"
        );
        let current = serde_json::to_vec(&stored).unwrap();
        assert!(!restore_query(&mut stored, &Callbacks(false)));
        assert_eq!(serde_json::to_vec(&stored).unwrap(), current);
    }
}

#[test]
fn retained_ordinary_calls_and_registered_callbacks_are_not_native_patterns() {
    for source in [
        "like('a','a')",
        "NOT regex_match_op('a','a')",
        "similar_to('a','a')",
    ] {
        let mut stored = expression(source);
        let before = stored.clone();
        assert!(!restore_expression(&mut stored, &Callbacks(true)).unwrap());
        assert_eq!(stored, before);
        let mut stored = query(source);
        let before = serde_json::to_vec(&stored).unwrap();
        assert!(!restore_query(&mut stored, &Callbacks(true)));
        assert_eq!(serde_json::to_vec(&stored).unwrap(), before);
    }
    for source in [
        "public.like('a','a')",
        "pg_catalog.like('a','a')",
        "like('a')",
        "like(DISTINCT 'a','a')",
    ] {
        let mut stored = expression(source);
        let before = stored.clone();
        assert!(!restore_expression(&mut stored, &Callbacks(false)).unwrap());
        assert_eq!(stored, before);
    }
    for builtin in [false, true] {
        let mut stored = expression("like('a','a')");
        let Expr::Func { binding, .. } = &mut stored else {
            unreachable!()
        };
        *binding = Some(FunctionBinding {
            name: "public.like".into(),
            argument_types: vec!["text".into(), "text".into()],
            builtin,
            object_id: (!builtin).then_some([7; 16]),
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        });
        let before = stored.clone();
        assert!(!restore_expression(&mut stored, &Callbacks(false)).unwrap());
        assert_eq!(stored, before);
    }
}

#[test]
fn retained_subqueries_and_statement_bodies_restore_before_syntax_site_binding() {
    let legacy = "SELECT NOT like((SELECT 'abc'), 'b%'), similar_to('a_b','a!_b','!')";
    let native = "SELECT (SELECT 'abc') NOT LIKE 'b%', 'a_b' SIMILAR TO 'a!_b' ESCAPE '!'";
    let mut statement = crate::compile(legacy).unwrap().remove(0);
    assert!(restore_statement(&mut statement, &Callbacks(false)).unwrap());
    assert_eq!(
        serde_json::to_value(&statement).unwrap(),
        serde_json::to_value(crate::compile(native).unwrap().remove(0)).unwrap()
    );
    let bytes = serde_json::to_vec(&statement).unwrap();
    assert!(!restore_statement(&mut statement, &Callbacks(false)).unwrap());
    assert_eq!(serde_json::to_vec(&statement).unwrap(), bytes);
    let mut plan = crate::plan::ExpressionPlan::lower(expression("NOT like((SELECT 'abc'), 'b%')"));
    assert!(restore_expression_plan(&mut plan, &Callbacks(false)));
    let expected = crate::plan::ExpressionPlan::lower(expression("(SELECT 'abc') NOT LIKE 'b%'"));
    assert_eq!(
        serde_json::to_vec(&plan).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    assert!(!restore_expression_plan(&mut plan, &Callbacks(false)));
}
