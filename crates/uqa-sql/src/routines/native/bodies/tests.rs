//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn source() -> SQLUserFunction {
    let Statement::CreateFunction(definition) = crate::compile(
        "CREATE FUNCTION public.saved(n integer) RETURNS integer LANGUAGE SQL AS 'uncompiled $77 and missing(n)'",
    ).unwrap().remove(0) else { panic!("routine declaration") };
    SQLUserFunction::new(*definition, RoutineBody::Source)
}

fn captured(syntax: &str, compiled: &str) -> SQLUserFunction {
    let mut routine = source();
    routine.def.body = FunctionBody::Statements(crate::compile(syntax).unwrap());
    routine.body = RoutineBody::Bound(
        CompiledFunctionBody::SQL(
            crate::compile(compiled)
                .unwrap()
                .into_iter()
                .map(UnifiedPlan::lower)
                .collect(),
        )
        .shared(),
    );
    routine
}

fn image(routine: &SQLUserFunction) -> Vec<u8> {
    let plans = match &routine.body {
        RoutineBody::Source => None,
        RoutineBody::Bound(body) => match &***body {
            CompiledFunctionBody::SQL(plans) => Some(plans),
            CompiledFunctionBody::PLpgSQL(_) => panic!("session body"),
        },
    };
    serde_json::to_vec(&(&routine.def, plans)).unwrap()
}

fn check(routine: &SQLUserFunction) -> Result<(), SQLError> {
    let before = image(routine);
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let result = validate_captured_body(routine, &ProductionControl::new(&memory, &token, &token));
    assert_eq!(memory.used(), 0);
    assert_eq!(image(routine), before);
    result
}

fn corrupt_parameters<T: serde::Serialize + serde::de::DeserializeOwned>(
    input: &T,
    ordinal: usize,
) -> T {
    fn corrupt(node: &mut serde_json::Value, ordinal: usize) -> usize {
        match node {
            serde_json::Value::Object(object) if object.contains_key("Param") => {
                object.insert("Param".into(), ordinal.into());
                1
            }
            serde_json::Value::Object(object) => {
                object.values_mut().map(|node| corrupt(node, ordinal)).sum()
            }
            serde_json::Value::Array(array) => {
                array.iter_mut().map(|node| corrupt(node, ordinal)).sum()
            }
            _ => 0,
        }
    }
    let mut serialized = serde_json::to_value(input).unwrap();
    assert!(corrupt(&mut serialized, ordinal) > 0);
    serde_json::from_value(serialized).unwrap()
}

#[test]
fn captured_routine_parameter_scope_checks_each_original_syntax_and_plan_owner() {
    for sql in [
        "SELECT $1",
        "WITH x AS (SELECT $1 AS id) SELECT (SELECT $1 FROM x) FROM x",
        "SELECT sum($1) OVER (ORDER BY $1 ROWS BETWEEN $1 PRECEDING AND CURRENT ROW)",
        "INSERT INTO public.items SELECT $1 RETURNING $1",
        "UPDATE public.items SET id=$1 WHERE id=(SELECT $1) RETURNING $1",
        "DELETE FROM public.items WHERE id=$1 RETURNING $1",
        "MERGE INTO public.items AS t USING (VALUES($1)) AS s(id) ON t.id=s.id WHEN MATCHED THEN UPDATE SET id=$1 WHEN NOT MATCHED THEN INSERT(id) VALUES($1) RETURNING $1",
    ] {
        check(&captured(sql, sql)).unwrap();
        for bad in [0, 2] {
            let original = captured(sql, sql);
            let mut syntax = original.clone();
            syntax.def.body = corrupt_parameters(&syntax.def.body, bad);
            let RoutineBody::Bound(body) = &original.body else {
                panic!("captured body")
            };
            let CompiledFunctionBody::SQL(plans) = &***body else {
                panic!("SQL body")
            };
            let mut plan = original.clone();
            plan.body = RoutineBody::Bound(
                CompiledFunctionBody::SQL(corrupt_parameters(plans, bad)).shared(),
            );
            for routine in [syntax, plan] {
                let error = check(&routine).unwrap_err();
                assert!(error.to_string().contains("parameter ordinal"), "{sql}: {error}");
            }
        }
    }
    for index in [0, 1, 2] {
        let mut routine = captured("SELECT $1", "SELECT $1");
        let mut plan = UnifiedPlan::lower(crate::compile("SELECT $1").unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |expression| {
            if matches!(expression, ScalarExpr::Param(_)) {
                *expression = ScalarExpr::TypedLiteral {
                    value: uqa_core::Value::Int(5),
                    ty: "integer".into(),
                    collation_oid: None,
                    composite_source: None,
                    bound_type: None,
                    parameter_index: Some(index),
                };
            }
        });
        routine.body = RoutineBody::Bound(CompiledFunctionBody::SQL(vec![plan]).shared());
        assert_eq!(check(&routine).is_ok(), index == 1);
    }
}

#[test]
fn captured_routine_source_bodies_remain_lazy_and_defaults_have_no_parameter_scope() {
    for language in ["sql", "plpgsql"] {
        let mut routine = source();
        routine.def.language = language.into();
        check(&routine).unwrap();
        routine.def.params[0].default = Some(Expr::Literal(uqa_core::Value::Int(3)));
        check(&routine).unwrap();
        for index in [0, 1, 77] {
            routine.def.params[0].default = Some(Expr::Param(index));
            assert!(check(&routine)
                .unwrap_err()
                .to_string()
                .contains("parameter ordinal"));
        }
    }
}

#[test]
fn captured_routine_body_form_statement_count_and_kind_must_match_the_declaration() {
    let mut empty = captured("", "");
    empty.def.is_procedure = true;
    empty.def.returns = crate::ast::FunctionReturns::None;
    check(&empty).unwrap();
    for (syntax, compiled) in [
        ("SELECT 1", "SELECT 1; SELECT 2"),
        ("SELECT 1; SELECT 2", "SELECT 1"),
        ("SELECT 1", "CREATE TABLE unexpected(id integer)"),
        ("SELECT 1", "DELETE FROM public.items"),
        (
            "INSERT INTO public.items VALUES(1)",
            "UPDATE public.items SET id=1",
        ),
    ] {
        check(&captured(syntax, compiled)).expect_err("inconsistent native body");
    }
    let mut routine = captured("SELECT $1", "SELECT $1");
    routine.def.language = "plpgsql".into();
    assert!(check(&routine)
        .unwrap_err()
        .to_string()
        .contains("language"));
    routine.def.language = "sql".into();
    routine.body = RoutineBody::Source;
    assert!(check(&routine)
        .unwrap_err()
        .to_string()
        .contains("body form"));
    let mut routine = source();
    routine.body = RoutineBody::Bound(CompiledFunctionBody::SQL(Vec::new()).shared());
    assert!(check(&routine)
        .unwrap_err()
        .to_string()
        .contains("body form"));
}

#[test]
fn captured_routine_parameter_slots_exclude_outputs_and_preserve_input_modes() {
    use crate::ast::FunctionParamMode;
    for procedure in [false, true] {
        for mode in [
            FunctionParamMode::In,
            FunctionParamMode::InOut,
            FunctionParamMode::Variadic,
        ] {
            let mut routine = captured("SELECT $1", "SELECT $1");
            routine.def.is_procedure = procedure;
            routine.def.params[0].mode = mode;
            let mut output = routine.def.params[0].clone();
            output.mode = FunctionParamMode::Out;
            output.name = "output".into();
            routine.def.params.insert(0, output);
            check(&routine).unwrap();
            let mut invalid = routine.clone();
            invalid.body = captured("SELECT $2", "SELECT $2").body;
            assert!(check(&invalid)
                .unwrap_err()
                .to_string()
                .contains("parameter ordinal"));
        }
    }
}

#[test]
fn captured_routine_body_validation_retires_denied_and_cancelled_scratch() {
    let routine = captured("SELECT (SELECT $1)", "SELECT (SELECT $1)");
    let before = image(&routine);
    let token = CancellationToken::new();
    let mut outcomes = [0, 0];
    for limit in [0, 64, 512, 64 << 10] {
        let memory = MemoryBudget::new(limit);
        match validate_captured_body(&routine, &ProductionControl::new(&memory, &token, &token)) {
            Ok(()) => outcomes[0] += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                outcomes[1] += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(outcomes.iter().all(|count| *count > 0));
    for original in [false, true] {
        let memory = MemoryBudget::new(64 << 10);
        let source = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original {
            source.cancel();
        } else {
            invoking.cancel();
        }
        let error = validate_captured_body(
            &routine,
            &ProductionControl::new(&memory, &source, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(image(&routine), before);
}
