//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::{Expr, FunctionDispatch},
    plan::ExpressionPlan,
    type_resolution::bind_type_introspection_with_control,
    RowSchema, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn source() -> Expr {
    let crate::Statement::Select(query) =
        crate::compile("SELECT json_extract_path('{\"a\":{\"b\":1}}', 'a', 'b')")
            .unwrap()
            .remove(0)
    else {
        panic!()
    };
    query.projections[0].expr.clone()
}

#[test]
fn invalid_json_path_binding_retains_a_typed_error_instead_of_panicking() {
    let crate::Statement::Select(query) =
        crate::compile("SELECT json_extract_path('{}'::json, true)")
            .unwrap()
            .remove(0)
    else {
        panic!()
    };
    let scalar = ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    let bound = crate::type_resolution::bind_type_introspection(scalar, &RowSchema::default(), &[]);
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = bound
    else {
        panic!("missing failed binding")
    };
    let error = binding.resolution_error.unwrap().sql_error();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function json_extract_path(json, boolean) does not exist"
    );
}

#[test]
fn json_path_normalization_admits_packed_arrays_and_releases_every_partial_owner() {
    let source = source();
    for allowance in [0, 32, 128, 512, 1024, 4096, 16384] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let (scalar, memory) =
            ExpressionPlan::lower_column_budgeted(&source, &budget, &token, &token)
                .unwrap()
                .into_parts();
        let source = control.finish(scalar, Some(memory)).unwrap();
        let held = budget
            .reserve(budget.limit() - budget.used() - allowance)
            .unwrap();
        match bind_type_introspection_with_control(source, &RowSchema::default(), &[], &control) {
            Ok(bound) => {
                let ScalarExpr::Func {
                    args,
                    binding: Some(binding),
                    ..
                } = &*bound
                else {
                    panic!()
                };
                assert_eq!(
                    binding.dispatch,
                    Some(FunctionDispatch::JsonPath {
                        as_text: false,
                        jsonb: false
                    })
                );
                assert_eq!(binding.argument_types, ["json", "text[]"]);
                assert!(binding.invocation.is_none());
                let ScalarExpr::Array(path) = &args[1] else {
                    panic!("expanded path was not packed")
                };
                assert_eq!(path.len(), 2);
                assert!(path
                    .iter()
                    .all(|part| matches!(part, ScalarExpr::Cast { .. })));
                assert_eq!(budget.used(), held.bytes() + bound.reserved_bytes());
                drop(bound);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn json_path_binding_observes_both_cancellation_owners() {
    for query_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let query = CancellationToken::new();
        let work = CancellationToken::new();
        let control = ProductionControl::new(&budget, &query, &work);
        let (scalar, memory) =
            ExpressionPlan::lower_column_budgeted(&source(), &budget, &query, &work)
                .unwrap()
                .into_parts();
        let source = control.finish(scalar, Some(memory)).unwrap();
        if query_cancelled {
            query.cancel();
        } else {
            work.cancel();
        }
        let error =
            bind_type_introspection_with_control(source, &RowSchema::default(), &[], &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn json_path_migration_respects_callback_precedence_without_losing_native_bindings() {
    struct Callbacks;
    impl crate::FunctionTypeResolver for Callbacks {
        fn has_untyped_function(&self, name: &str) -> bool {
            name == "json_extract_path"
        }

        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<crate::ColumnType>],
            _: bool,
        ) -> Result<Option<crate::ColumnType>, crate::SQLError> {
            Ok(None)
        }
    }
    let selected = crate::type_resolution::bind_type_introspection(
        ExpressionPlan::lower(source()).scalar,
        &RowSchema::default(),
        &[],
    );
    let ScalarExpr::Func {
        binding: Some(mut binding),
        ..
    } = selected
    else {
        panic!("selected native call")
    };
    let name = "json_extract_path";
    assert!(super::requires_binding(name, None, None));
    assert!(!super::requires_binding(name, None, Some(&Callbacks)));
    assert!(super::requires_binding(
        "pg_catalog.json_extract_path",
        None,
        Some(&Callbacks)
    ));
    assert!(!super::requires_binding(
        name,
        Some(&binding),
        Some(&Callbacks)
    ));
    binding.dispatch = None;
    assert!(super::requires_binding(
        name,
        Some(&binding),
        Some(&Callbacks)
    ));
}
