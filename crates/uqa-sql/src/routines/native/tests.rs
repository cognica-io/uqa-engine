//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionDispatch, FunctionResolutionError, Statement};

fn definition() -> CreateFunction {
    let Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION public.f(integer) RETURNS integer LANGUAGE SQL AS 'SELECT missing($1)'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    definition.object_id = Some([1; 16]);
    *definition
}

fn binding() -> FunctionBinding {
    FunctionBinding {
        object_id: Some([1; 16]),
        name: "public.old_name".into(),
        argument_types: vec!["int4".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    }
}

#[test]
fn native_routine_references_preserve_identity_through_rename_and_reject_replacements() {
    let definition = definition();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        references.binding(&binding()).unwrap();
        let mut missing = binding();
        missing.object_id = Some([2; 16]);
        missing.name = "public.f".into();
        assert!(references.binding(&missing).is_err());
        let mut changed = binding();
        changed.argument_types[0] = "text".into();
        assert!(references.binding(&changed).is_err());
        assert!(references
            .identity([1; 16], &["int4".into()], true)
            .is_err());
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_reference_index_rejects_missing_zero_and_duplicate_identities() {
    let mut definition = definition();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    for identity in [None, Some([0; 16])] {
        definition.object_id = identity;
        assert!(
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).is_err()
        );
        assert_eq!(memory.used(), 0);
    }
    definition.object_id = Some([1; 16]);
    assert!(
        NativeRoutineReferences::new([&definition, &definition].into_iter(), &memory, &token)
            .is_err()
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_references_keep_late_calls_structural_dispatch_and_deferred_errors() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let references = NativeRoutineReferences::new(std::iter::empty(), &memory, &token).unwrap();
    for shape in 0..4 {
        let mut call = binding();
        match shape {
            0 => call.object_id = None,
            1 => call.builtin = true,
            2 => call.dispatch = Some(FunctionDispatch::FieldSelect),
            _ => {
                call.resolution_error =
                    Some(Box::new(FunctionResolutionError::UndefinedFunction {
                        signature: "f(integer)".into(),
                    }));
            }
        }
        references.binding(&call).unwrap();
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_references_walk_nested_schema_and_query_expressions_without_evaluation() {
    let definition = definition();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let references =
        NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
    let Statement::Select(query) =
        crate::compile("SELECT CASE WHEN false THEN public.f(1) ELSE 9 END")
            .unwrap()
            .remove(0)
    else {
        panic!("query");
    };
    let mut plan = crate::plan::QueryPlan::lower(*query);
    plan.rewrite_scalar_expressions(&mut |expression| {
        expression.visit_mut(&mut |node| {
            if let ScalarExpr::Func { binding: slot, .. } = node {
                *slot = Some(binding());
            }
        });
    });
    references.plan(PlanRef::Query(&plan)).unwrap();
    plan.rewrite_scalar_expressions(&mut |expression| {
        expression.visit_mut(&mut |node| {
            if let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = node
            {
                binding.object_id = Some([2; 16]);
            }
        });
    });
    assert!(references.plan(PlanRef::Query(&plan)).is_err());
    let expression = Expr::Func {
        name: "public.f".into(),
        binding: Some(binding()),
        args: vec![Expr::Literal(uqa_core::Value::Int(1))],
        distinct: false,
        order_by: vec![],
        order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
        filter: None,
    };
    references.expression(&expression).unwrap();
}

#[test]
fn native_routine_reference_admission_and_cancellation_release_original_workspace() {
    let definition = definition();
    let empty = MemoryBudget::new(0);
    let token = CancellationToken::new();
    assert!(NativeRoutineReferences::new(std::iter::once(&definition), &empty, &token).is_err());
    assert_eq!(empty.used(), 0);
    let memory = MemoryBudget::new(64 << 10);
    {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        token.cancel();
        assert!(references.binding(&binding()).is_err());
    }
    assert_eq!(memory.used(), 0);
    assert!(NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_references_include_from_functions_and_grouped_function_targets() {
    let definition = definition();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        for sql in [
            "SELECT * FROM public.f(1)",
            "SELECT * FROM ROWS FROM(public.f(1),public.f(2))",
        ] {
            let crate::plan::UnifiedPlan::Query(mut plan) =
                crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            for valid in [true, false] {
                let crate::plan::RelationalPlan::QueryBlock(block) = &mut plan.root else {
                    panic!("block");
                };
                let mut selected = binding();
                selected.object_id = Some([if valid { 1 } else { 2 }; 16]);
                match block.from.as_mut().unwrap() {
                    crate::plan::SourcePlan::Function { binding, .. } => *binding = Some(selected),
                    crate::plan::SourcePlan::FunctionGroup { functions, .. } => {
                        for function in functions {
                            function.binding = Some(selected.clone());
                        }
                    }
                    _ => panic!("function source"),
                }
                let retained = memory.used();
                assert_eq!(references.plan(PlanRef::Query(&plan)).is_ok(), valid);
                assert_eq!(memory.used(), retained);
            }
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_references_check_stored_actions_under_the_original_allowance() {
    let definition = definition();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        let mut call = binding();
        for valid in [true, false] {
            call.object_id = Some([if valid { 1 } else { 2 }; 16]);
            let statement = Statement::Values {
                rows: vec![vec![Expr::Func {
                    name: "public.f".into(),
                    binding: Some(call.clone()),
                    args: vec![Expr::Literal(uqa_core::Value::Int(1))],
                    distinct: false,
                    order_by: vec![],
                    order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
                    filter: None,
                }]],
            };
            let retained = memory.used();
            assert_eq!(references.statement(&statement).is_ok(), valid);
            assert_eq!(memory.used(), retained);
            let full = memory.reserve(memory.available()).unwrap();
            assert!(references.statement(&statement).is_err());
            drop(full);
            assert_eq!(memory.used(), retained);
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_trigger_references_keep_the_selected_contract_and_deferred_body() {
    for (parameters, returns, language, procedure, valid) in [
        ("", "trigger", "plpgsql", false, true),
        ("", "pg_catalog.trigger", "plpgsql", false, true),
        ("", "integer", "plpgsql", false, false),
        ("", "SETOF trigger", "plpgsql", false, true),
        ("", "trigger", "sql", false, false),
        ("integer", "trigger", "plpgsql", false, false),
        ("", "trigger", "plpgsql", true, false),
    ] {
        let Statement::CreateFunction(mut definition) = crate::compile(&format!(
            "CREATE FUNCTION public.renamed({parameters}) RETURNS {returns} LANGUAGE {language} AS 'uncompiled source body'",
        )).unwrap().remove(0) else { panic!("function"); };
        definition.object_id = Some([1; 16]);
        definition.is_procedure = procedure;
        let memory = MemoryBudget::new(64 << 10);
        let token = CancellationToken::new();
        {
            let references =
                NativeRoutineReferences::new(std::iter::once(&*definition), &memory, &token)
                    .unwrap();
            let result = references.trigger("public.historical_name", Some([1; 16]));
            assert_eq!(
                result.is_ok(),
                valid,
                "{parameters}/{returns}/{language}/{procedure}: {result:?}"
            );
            assert!(references.trigger("public.renamed", Some([2; 16])).is_err());
            token.cancel();
            assert!(references
                .trigger("public.historical_name", Some([1; 16]))
                .is_err());
        }
        assert_eq!(memory.used(), 0);
    }
}

fn trigger_definition(name: &str, identity: u8) -> CreateFunction {
    let Statement::CreateFunction(mut definition) = crate::compile(&format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS 'uncompiled source body'",
    ))
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    definition.object_id = Some([identity; 16]);
    *definition
}

#[test]
fn native_legacy_trigger_names_preserve_quoted_identity_without_rebinding() {
    for (stored, requested) in [
        ("public.fire", r#""public"."fire""#),
        (r#""s.q"."f""x""#, r#""s.q"."f""x""#),
    ] {
        let definition = trigger_definition(stored, 1);
        let memory = MemoryBudget::new(64 << 10);
        let token = CancellationToken::new();
        {
            let references =
                NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token)
                    .unwrap();
            references.trigger(requested, None).unwrap();
            references
                .trigger("historical_name", Some([1; 16]))
                .unwrap();
            assert!(references.trigger("public.absent", None).is_err());
            assert!(references.trigger("fire", None).is_err());
            assert!(references.trigger(requested, Some([2; 16])).is_err());
        }
        assert_eq!(definition.object_id, Some([1; 16]));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_legacy_trigger_names_reject_ambiguous_zero_argument_functions() {
    let original = trigger_definition("public.fire", 1);
    let duplicate = trigger_definition(r#""public"."fire""#, 2);
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    assert!(
        NativeRoutineReferences::new([&original, &duplicate].into_iter(), &memory, &token).is_err()
    );
    assert_eq!(memory.used(), 0);
    let mut overload = definition();
    overload.name = original.name.clone();
    overload.object_id = Some([2; 16]);
    {
        let references =
            NativeRoutineReferences::new([&original, &overload].into_iter(), &memory, &token)
                .unwrap();
        references.trigger("public.fire", None).unwrap();
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_legacy_trigger_name_index_retires_partial_admission_and_cancellation() {
    let definition = trigger_definition("public.fire", 1);
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let used = {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        references.trigger("public.fire", None).unwrap();
        memory.used()
    };
    assert_eq!(memory.used(), 0);
    let denied = MemoryBudget::new(used - 1);
    assert!(NativeRoutineReferences::new(std::iter::once(&definition), &denied, &token).is_err());
    assert_eq!(denied.used(), 0);
    {
        let references =
            NativeRoutineReferences::new(std::iter::once(&definition), &memory, &token).unwrap();
        token.cancel();
        assert!(references.trigger("public.fire", None).is_err());
    }
    assert_eq!(memory.used(), 0);
}
