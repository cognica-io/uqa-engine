//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn ordinary_within_group_resolves_ordered_arguments_before_modifiers_and_inputs() {
    for (sql, code, message) in [
        (
            "SELECT abs() WITHIN GROUP (ORDER BY 1)",
            "42809",
            "WITHIN GROUP specified, but abs is not an aggregate function",
        ),
        (
            "SELECT abs(1) WITHIN GROUP (ORDER BY 2)",
            "42883",
            "function abs(integer, integer) does not exist",
        ),
        (
            "SELECT lower() WITHIN GROUP (ORDER BY 'ABC')",
            "42809",
            "WITHIN GROUP specified, but lower is not an aggregate function",
        ),
        (
            "SELECT sum() WITHIN GROUP (ORDER BY 1)",
            "42809",
            "sum is not an ordered-set aggregate, so it cannot have WITHIN GROUP",
        ),
        (
            "SELECT percentile_cont(0.5, 1)",
            "42809",
            "WITHIN GROUP is required for ordered-set aggregate percentile_cont",
        ),
        (
            "SELECT mode(1) WITHIN GROUP (ORDER BY 1)",
            "42883",
            "function mode(integer, integer) does not exist",
        ),
        (
            "SELECT absent() WITHIN GROUP (ORDER BY 1) FILTER (WHERE 1)",
            "42804",
            "argument of FILTER must be type boolean, not type integer",
        ),
        (
            "SELECT abs() WITHIN GROUP (ORDER BY 'bad'::integer)",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some(code), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
}

#[test]
fn ordered_set_direct_parameter_takes_the_selected_declared_type() {
    let plan = UnifiedPlan::lower(
        crate::compile(
            "SELECT percentile_cont($1) WITHIN GROUP (ORDER BY id) FROM assignment_target",
        )
        .unwrap()
        .remove(0),
    );
    let parameters =
        infer_prepared_parameter_types(&NoRoutines, &plan, &[None], &assignment_context()).unwrap();
    assert_eq!(parameters, vec![Some(ColumnType::DoublePrecision)]);
}

#[test]
fn legacy_ordered_set_identity_requires_initial_catalog_binding() {
    for (sql, ordered_set) in [
        ("SELECT mode() WITHIN GROUP (ORDER BY 1)", true),
        (
            "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY 1)",
            true,
        ),
        ("SELECT array_agg(1 ORDER BY 1)", false),
    ] {
        let crate::Statement::Select(statement) = crate::compile(sql).unwrap().remove(0) else {
            unreachable!()
        };
        let mut plan = crate::plan::QueryPlan::lower(*statement);
        assert!(!crate::binding::view_dependencies::query_plan_has_legacy_routine_identity(&plan));
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { order_syntax, .. } = expression {
                *order_syntax = crate::ast::FunctionOrderSyntax::Legacy;
            }
        });
        assert_eq!(
            crate::binding::view_dependencies::query_plan_has_legacy_routine_identity(&plan),
            ordered_set
        );
    }
}

#[test]
fn analyzed_fixed_calls_retain_their_declaration_before_optimization() {
    for sql in [
        "SELECT lower('HELLO') WHERE false",
        "SELECT lower(id::text) FROM assignment_target",
        "SELECT 1 FROM assignment_target WINDOW unused AS (ORDER BY lower(id::text))",
        "UPDATE assignment_target SET id=id RETURNING lower(id::text)",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        read_prepared_inputs(&NoRoutines, &mut plan, &[], &assignment_context(), None).unwrap();
        let mut observed = false;
        plan.visit_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { name, binding, .. } = expression {
                if name == "lower" {
                    let binding = binding.as_ref().expect("analyzed fixed declaration");
                    assert_eq!(binding.name, "pg_catalog.lower");
                    assert_eq!(binding.argument_types, ["text"]);
                    observed = true;
                }
            }
        });
        assert!(observed, "{sql}");
    }
}

#[test]
fn preparation_preserves_untyped_callback_identity_and_original_arguments() {
    struct Callbacks;
    impl FunctionTypeResolver for Callbacks {
        fn has_untyped_function(&self, name: &str) -> bool {
            name == "pg_get_viewdef"
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
    impl RoutineResolution for Callbacks {
        fn has_registered_scalar_function(&self, name: &str) -> bool {
            self.has_untyped_function(name)
        }
    }

    for sql in [
        "SELECT pg_get_viewdef(0::oid)",
        "SELECT pg_get_viewdef(true)",
        "SELECT pg_get_viewdef('callback text, not oid input')",
        "UPDATE assignment_target SET id=id RETURNING pg_get_viewdef('callback text')",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        read_prepared_inputs(&Callbacks, &mut plan, &[], &assignment_context(), None).unwrap();
        let mut observed = false;
        plan.visit_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Func { name, binding, .. } = expression {
                if name == "pg_get_viewdef" {
                    assert!(binding.is_none(), "{sql}");
                    observed = true;
                }
            }
        });
        assert!(observed, "{sql}");
    }
    let mut plan = UnifiedPlan::lower(
        crate::compile("SELECT pg_catalog.pg_get_viewdef(0::oid)")
            .unwrap()
            .remove(0),
    );
    read_prepared_inputs(&Callbacks, &mut plan, &[], &assignment_context(), None).unwrap();
    plan.visit_scalar_expressions(&mut |expression| {
        if let ScalarExpr::Func { binding, .. } = expression {
            assert!(binding.as_ref().is_some_and(|binding| binding.builtin));
        }
    });
}
