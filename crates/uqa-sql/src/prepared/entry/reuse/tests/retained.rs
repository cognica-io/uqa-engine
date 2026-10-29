//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral};

#[derive(Debug)]
struct UnreadConstant;

impl ConstantValueSource for UnreadConstant {
    fn read(&self, _: u64) -> Result<Value, crate::SQLError> {
        panic!("dependency classification must not materialize a retained value")
    }
}

fn literal(ty: Option<ColumnType>, declaration: Option<&str>) -> ScalarExpr {
    ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(Arc::new(UnreadConstant), 0),
        ty,
        declaration.map(str::to_owned),
        None,
    ))
}

#[test]
fn retained_constants_preserve_prepared_dependency_identity_without_payload_reads() {
    for mut candidate in [
        entry("SELECT lower('HELLO') AS value", "SELECT 'hello' AS value"),
        mixed_entry(),
    ] {
        let mut freeze = |expression: &mut ScalarExpr| {
            if matches!(expression, ScalarExpr::Literal(Value::Str(_))) {
                *expression = literal(Some(ColumnType::Text), Some("text"));
            }
        };
        rewrite_fixture(&mut candidate, &mut freeze);
        let mut source_plan = candidate.source_plan.as_ref().clone();
        source_plan.rewrite_scalar_expressions(&mut freeze);
        candidate.source_plan = source_plan.into();
        let mut logical_plan = candidate.logical_plan.as_ref().clone();
        logical_plan.rewrite_scalar_expressions(&mut freeze);
        candidate.logical_plan = logical_plan.into();
        assert!(candidate.has_tracked_executable_dependencies());
    }
}

#[test]
fn retained_constant_declarations_keep_unknown_and_catalog_dependencies_conservative() {
    for (ty, declaration, tracked) in [
        (Some(ColumnType::Text), None, true),
        (
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            None,
            true,
        ),
        (Some(domain()), None, false),
        (Some(ColumnType::Regclass), None, false),
        (Some(ColumnType::Record), None, false),
        (Some(ColumnType::Timestamp), None, false),
        (
            Some(ColumnType::Named("public.changed".into())),
            None,
            false,
        ),
        (Some(ColumnType::Text), Some("public.changed"), false),
        (Some(ColumnType::Text), Some("regclass"), false),
        (None, None, false),
        (None, Some("text"), false),
    ] {
        let mut candidate = entry("SELECT lower('HELLO') AS value", "SELECT 'hello' AS value");
        rewrite_fixture(&mut candidate, &mut |expression| {
            if matches!(expression, ScalarExpr::Literal(_)) {
                *expression = literal(ty.clone(), declaration);
            }
        });
        assert_eq!(
            candidate.has_tracked_executable_dependencies(),
            tracked,
            "{ty:?}, {declaration:?}"
        );
    }
}
