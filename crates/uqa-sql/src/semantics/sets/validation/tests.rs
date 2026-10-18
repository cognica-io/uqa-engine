//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::resolve_set_function_binding;
use crate::ast::{ColumnType, FunctionBinding};
use crate::{FunctionTypeResolver, RowSchema, SQLError, ScalarExpr};
use uqa_core::Value;

struct NoRoutines;

impl FunctionTypeResolver for NoRoutines {
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

impl crate::routines::RoutineResolution for NoRoutines {}

impl crate::plan::AggregateClassifier for NoRoutines {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

fn resolve(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
) -> Result<Option<FunctionBinding>, SQLError> {
    let schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Text)]);
    resolve_set_function_binding(&NoRoutines, &NoRoutines, name, binding, args, &schema, &[])
}

fn text(value: &str) -> ScalarExpr {
    ScalarExpr::Literal(Value::Str(value.into()))
}

#[test]
fn unbound_stored_set_calls_select_the_catalog_declaration_and_bound_calls_keep_theirs() {
    // A definition stored before these signatures existed records the call without a binding.
    let arguments = [ScalarExpr::Column("x".into()), text("[a-z]"), text("g")];
    let selected = resolve("regexp_matches", None, &arguments)
        .unwrap()
        .unwrap();
    assert_eq!(selected.name, "pg_catalog.regexp_matches");
    assert_eq!(selected.argument_types, ["text", "text", "text"]);
    assert!(selected.builtin);
    let split = resolve("regexp_split_to_table", None, &arguments[..2])
        .unwrap()
        .unwrap();
    assert_eq!(split.name, "pg_catalog.regexp_split_to_table");
    assert_eq!(split.argument_types, ["text", "text"]);
    // A recorded built-in binding stays with the caller.
    assert_eq!(
        resolve("regexp_matches", Some(&selected), &arguments).unwrap(),
        None
    );
    // A set function outside the fixed signatures stays unbound.
    assert_eq!(
        resolve("json_array_elements", None, &[text("[1]")]).unwrap(),
        None
    );
    let error = resolve(
        "regexp_matches",
        None,
        &[ScalarExpr::Literal(Value::Int(123)), text("2")],
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function regexp_matches(integer, unknown) does not exist"
    );
}
