//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct EmptyResolver;
impl FunctionTypeResolver for EmptyResolver {
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

fn binding(name: &str, types: &[&str]) -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: name.into(),
        argument_types: types.iter().map(|ty| (*ty).into()).collect(),
        builtin: true,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    }
}

fn marked(dispatch: FunctionDispatch, args: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Func {
        name: dispatch.label().into(),
        binding: Some(FunctionBinding::dispatched(dispatch)),
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn named(name: &str, value: ScalarExpr) -> ScalarExpr {
    marked(
        FunctionDispatch::NamedArgument,
        vec![ScalarExpr::Literal(Value::Str(name.into())), value],
    )
}

fn assert_matches_candidate_path(
    name: &str,
    selected: FunctionBinding,
    arguments: Vec<ScalarExpr>,
) -> (Option<FunctionBinding>, Vec<ScalarExpr>) {
    let schema = crate::RowSchema::default();
    let mut fast_binding = Some(selected.clone());
    let mut ordinary_binding = Some(selected);
    let mut fast = arguments.clone();
    let mut ordinary = arguments;
    let fast_name = bind_call(
        name.into(),
        &mut fast_binding,
        &mut fast,
        &schema,
        &[],
        None,
    );
    let ordinary_name = bind_call(
        name.into(),
        &mut ordinary_binding,
        &mut ordinary,
        &schema,
        &[],
        Some(&EmptyResolver),
    );
    assert_eq!(fast_name, ordinary_name);
    assert_eq!(fast_binding, ordinary_binding);
    assert_eq!(fast, ordinary);
    (fast_binding, fast)
}

#[test]
fn selected_descriptor_keeps_named_default_and_unknown_literal_coercions() {
    let selected = binding("pg_catalog.jsonb_strip_nulls", &["jsonb", "boolean"]);
    let (_, args) = assert_matches_candidate_path(
        "jsonb_strip_nulls",
        selected.clone(),
        vec![ScalarExpr::Literal(Value::Str("{}".into()))],
    );
    assert_eq!(args.len(), 2);
    assert_eq!(args[1], ScalarExpr::Literal(Value::Bool(false)));
    assert!(matches!(&args[0], ScalarExpr::Cast { ty, .. } if ty == "jsonb"));
    let (_, args) = assert_matches_candidate_path(
        "jsonb_strip_nulls",
        selected,
        vec![
            named("strip_in_arrays", ScalarExpr::Literal(Value::Bool(true))),
            named("target", ScalarExpr::Literal(Value::Str("[]".into()))),
        ],
    );
    assert_eq!(args[1], ScalarExpr::Literal(Value::Bool(true)));
    let (_, args) = assert_matches_candidate_path(
        "abs",
        binding("pg_catalog.abs", &["integer"]),
        vec![ScalarExpr::Literal(Value::Null)],
    );
    assert!(matches!(&args[0], ScalarExpr::Cast { ty, .. } if ty == "integer"));
}

#[test]
fn selected_descriptor_preserves_invalid_binding_and_variadic_diagnostics() {
    assert_matches_candidate_path(
        "abs",
        binding("pg_catalog.abs", &["integer"]),
        vec![ScalarExpr::Literal(Value::Bool(true))],
    );
    assert_matches_candidate_path(
        "abs",
        binding("pg_catalog.length", &["text"]),
        vec![ScalarExpr::Literal(Value::Str("text".into()))],
    );
    let variadic = marked(
        FunctionDispatch::VariadicArgument,
        vec![ScalarExpr::Literal(Value::Str("{}".into()))],
    );
    assert_matches_candidate_path(
        "jsonb_strip_nulls",
        binding("pg_catalog.jsonb_strip_nulls", &["jsonb", "boolean"]),
        vec![named("target", variadic)],
    );
    assert_matches_candidate_path(
        "abs",
        binding("abs", &["integer"]),
        vec![ScalarExpr::Literal(Value::Int(1))],
    );
}
