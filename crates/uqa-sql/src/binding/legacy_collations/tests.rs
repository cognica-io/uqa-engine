//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{call_lacks_input_collation, expression_lacks_input_collation};
use crate::ast::{Expr, FunctionBinding};
use crate::catalog::collations::CollationSelection;

/// A binding as an earlier release serialized it, without an input collation.
fn stored(json: &str) -> FunctionBinding {
    serde_json::from_str(json).unwrap()
}

#[test]
fn collation_dependent_calls_stored_without_a_selection_are_found() {
    for json in [
        r#"{"name":"pg_catalog.lower","argument_types":["text"],"builtin":true}"#,
        r#"{"name":"pg_catalog.upper","argument_types":["text"],"builtin":true}"#,
        r#"{"name":"pg_catalog.casefold","argument_types":["text"],"builtin":true}"#,
        r#"{"name":"pg_catalog.starts_with","argument_types":["text","text"],"builtin":true}"#,
        r#"{"name":"pg_catalog.regexp_like","argument_types":["text","text"],"builtin":true}"#,
        r#"{"name":"ILIKE","argument_types":[],"builtin":true,"dispatch":{"Pattern":{"operator":"ILike","oid":1627}}}"#,
        r#"{"name":"~","argument_types":[],"builtin":true,"dispatch":{"Pattern":{"operator":"Regex","oid":641}}}"#,
        r#"{"name":"!~*","argument_types":[],"builtin":true,"dispatch":{"Pattern":{"operator":"NotRegexInsensitive","oid":1229}}}"#,
    ] {
        let mut binding = stored(json);
        assert!(
            call_lacks_input_collation(&binding.name, Some(&binding)),
            "{json}"
        );
        binding.input_collation = CollationSelection::from_oid(950);
        assert!(
            !call_lacks_input_collation(&binding.name, Some(&binding)),
            "{json}"
        );
        binding.input_collation = Some(CollationSelection::INDETERMINATE);
        assert!(
            !call_lacks_input_collation(&binding.name, Some(&binding)),
            "{json}"
        );
    }
    // An unbound call by a name that has no other overload was stored by an earlier release.
    for name in [
        "regexp_like",
        "pg_catalog.regexp_matches",
        "initcap",
        "casefold",
        "starts_with",
    ] {
        assert!(call_lacks_input_collation(name, None), "{name}");
    }
}

#[test]
fn calls_whose_results_do_not_depend_on_the_collation_are_not_found() {
    for json in [
        // LIKE compares characters exactly under every built-in collation.
        r#"{"name":"LIKE","argument_types":[],"builtin":true,"dispatch":{"Pattern":{"operator":"Like","oid":1209}}}"#,
        r#"{"name":"pg_catalog.length","argument_types":["text"],"builtin":true}"#,
        // A user routine named like a built-in.
        r#"{"name":"public.lower","argument_types":["text"],"builtin":false,"object_id":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]}"#,
    ] {
        let binding = stored(json);
        assert!(
            !call_lacks_input_collation(&binding.name, Some(&binding)),
            "{json}"
        );
    }
    // `lower` and `upper` of a range stay unbound.
    for name in ["lower", "upper", "length"] {
        assert!(!call_lacks_input_collation(name, None), "{name}");
    }
}

#[test]
fn nested_calls_are_found_in_stored_expressions() {
    let lower = stored(r#"{"name":"pg_catalog.lower","argument_types":["text"],"builtin":true}"#);
    let call = |binding: Option<FunctionBinding>, args: Vec<Expr>| Expr::Func {
        name: "lower".into(),
        binding,
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
    };
    let legacy = Expr::Not(Box::new(call(
        Some(lower.clone()),
        vec![Expr::Column("a".into())],
    )));
    assert!(expression_lacks_input_collation(&legacy));
    let mut derived = lower;
    derived.input_collation = CollationSelection::from_oid(950);
    assert!(!expression_lacks_input_collation(&Expr::Not(Box::new(
        call(Some(derived), vec![Expr::Column("a".into())])
    ))));
    assert!(!expression_lacks_input_collation(&call(
        None,
        vec![Expr::Column("range".into())]
    )));
}
