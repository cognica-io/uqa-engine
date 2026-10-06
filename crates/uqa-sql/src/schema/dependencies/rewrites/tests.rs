//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{rewrite_sequence_function_references, stored_relation_reference_matches};
use crate::ast::{Expr, FunctionBinding};
use uqa_core::{RelationIdentity, Value};

#[test]
fn legacy_relation_references_match_canonical_targets_and_corruption_fails_closed() {
    let public_parent = RelationIdentity::new("public", "parent");
    let app_parent = RelationIdentity::new("app", "parent");

    assert!(stored_relation_reference_matches("parent", &public_parent));
    assert!(stored_relation_reference_matches("parent", &app_parent));
    assert!(stored_relation_reference_matches(
        "public.parent",
        &public_parent
    ));
    assert!(!stored_relation_reference_matches(
        "public.parent",
        &app_parent
    ));
    assert!(stored_relation_reference_matches(
        "corrupt.reference.extra",
        &public_parent
    ));
}

#[test]
fn sequence_reference_rewriting_respects_selected_routines() {
    for (name, selected, builtin, expected) in [
        ("nextval", "app.nextval", false, false),
        ("nextval", "pg_catalog.lower", true, false),
        ("legacy_name", "pg_catalog.nextval", true, true),
    ] {
        let mut expression = Expr::Func {
            name: name.into(),
            binding: Some(FunctionBinding {
                object_id: (!builtin).then_some([7; 16]),
                name: selected.into(),
                argument_types: vec!["text".into()],
                builtin,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            }),
            args: vec![Expr::Literal(Value::Str("app.ids".into()))],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let mut visited = false;
        rewrite_sequence_function_references(&mut expression, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, expected, "{name}: {selected}");
    }
}
