//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionBinding;
use std::cell::RefCell;

#[derive(Default)]
struct References {
    calls: RefCell<Vec<String>>,
    unavailable: bool,
}

impl SchemaReferenceCatalog for References {
    fn loaded_relation_name(&self, _: &str) -> Result<Option<String>, String> {
        panic!("legacy sequence binding must use the stored sequence registry")
    }

    fn bound_relation_oid(&self, canonical: &str) -> Result<Option<i64>, String> {
        self.calls.borrow_mut().push(format!("oid:{canonical}"));
        Ok(Some(41))
    }

    fn visible_relation_oid(&self, _: &str) -> Result<Option<i64>, String> {
        panic!("legacy sequence binding must not consult the active search path")
    }

    fn sequence_for_binding(&self, _: &str) -> Result<String, String> {
        panic!("legacy sequence binding must not consult the active search path")
    }
}

impl StoredSequenceNames for References {
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(format!("sequence:{reference}"));
        if self.unavailable {
            Err("ambiguous persisted sequence reference `ids`".into())
        } else {
            Ok("app.ids".into())
        }
    }
}

fn call(name: &str, argument: Expr) -> Expr {
    Expr::Func {
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: Some(FunctionBinding {
            object_id: None,
            name: format!("pg_catalog.{name}"),
            argument_types: vec!["text".into()],
            builtin: true,
            dispatch: None,
            invocation: None,
            composite_field: None,
            resolution_error: None,
        }),
        args: vec![argument],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

#[test]
fn selected_legacy_sequence_inputs_are_bound_once_without_session_resolution() {
    for name in ["nextval", "currval", "setval"] {
        let references = References::default();
        let mut expression = call(name, Expr::Literal(Value::Str("ids".into())));
        assert!(
            bind_legacy_sequence_regclass_constants(&references, &references, &mut expression)
                .unwrap()
        );
        let Expr::Func { args, binding, .. } = &expression else {
            unreachable!()
        };
        assert_eq!(
            args[0],
            Expr::TypedLiteral {
                value: Value::Int(41),
                ty: "regclass".into()
            }
        );
        assert_eq!(binding.as_ref().unwrap().argument_types, ["regclass"]);
        assert!(!bind_legacy_sequence_regclass_constants(
            &references,
            &references,
            &mut expression
        )
        .unwrap());
        assert_eq!(*references.calls.borrow(), ["sequence:ids", "oid:app.ids"]);
    }
}

#[test]
fn legacy_sequence_conversion_preserves_explicit_text_and_user_or_unbound_calls() {
    let references = References::default();
    let mut expressions = vec![
        call(
            "nextval",
            Expr::Cast {
                implicit: false,
                expr: Box::new(Expr::Literal(Value::Str("ids".into()))),
                ty: "text".into(),
            },
        ),
        call(
            "nextval",
            Expr::TypedLiteral {
                value: Value::Str("ids".into()),
                ty: "text".into(),
            },
        ),
        call("nextval", Expr::Literal(Value::Str("ids".into()))),
        call("nextval", Expr::Literal(Value::Str("ids".into()))),
    ];
    let Expr::Func { binding, .. } = &mut expressions[2] else {
        unreachable!()
    };
    let binding = binding.as_mut().unwrap();
    binding.builtin = false;
    binding.object_id = Some([7; 16]);
    binding.name = "app.nextval".into();
    let Expr::Func { binding, .. } = &mut expressions[3] else {
        unreachable!()
    };
    *binding = None;
    for mut expression in expressions {
        let before = expression.clone();
        assert!(!bind_legacy_sequence_regclass_constants(
            &references,
            &references,
            &mut expression
        )
        .unwrap());
        assert_eq!(expression, before);
    }
    assert!(references.calls.borrow().is_empty());
}

#[test]
fn ambiguous_legacy_sequence_inputs_fail_without_rewriting_the_argument() {
    let references = References {
        unavailable: true,
        ..References::default()
    };
    let mut expression = call("nextval", Expr::Literal(Value::Str("ids".into())));
    let before = expression.clone();
    assert_eq!(
        bind_legacy_sequence_regclass_constants(&references, &references, &mut expression)
            .unwrap_err(),
        "ambiguous persisted sequence reference `ids`"
    );
    assert_eq!(expression, before);
    assert_eq!(*references.calls.borrow(), ["sequence:ids"]);
}
