//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine defaults use SQL's ordered analysis while retaining call-time evaluation.

use super::*;
use crate::plan::ExpressionPlan;

struct Aliases;
impl crate::schema::dependencies::oid_alias::OidAliasInput for Aliases {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        Ok(None)
    }
}

fn expression(text: &str) -> ExpressionPlan {
    let crate::Statement::CreateFunction(mut definition) = crate::compile(&format!(
        "CREATE FUNCTION f(v integer DEFAULT {text}) RETURNS integer LANGUAGE sql AS 'SELECT v'"
    ))
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    ExpressionPlan::lower(definition.params[0].default.take().unwrap())
}

#[test]
fn default_errors_follow_expression_analysis_order() {
    for (text, state, message) in [
        (
            "absent + (SELECT 1)",
            "42703",
            "column \"absent\" does not exist",
        ),
        (
            "ARRAY[absent,$1]",
            "42703",
            "column \"absent\" does not exist",
        ),
        ("sum(absent)", "42703", "column \"absent\" does not exist"),
        (
            "count('bad'::integer)",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "abs('bad') + absent",
            "22P02",
            "invalid input syntax for type double precision: \"bad\"",
        ),
        ("$1", "42P02", "there is no parameter $1"),
        (
            "(SELECT 1)",
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ),
        (
            "count(*)",
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ),
        (
            "row_number() OVER ()",
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ),
    ] {
        let error = analyze_default_inputs(
            &NoRoutines,
            &|_: &str| false,
            &Aliases,
            &mut expression(text),
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string()),
            (Some(state), message.into()),
            "{text}"
        );
    }
}

#[test]
fn default_analysis_reads_input_constants_without_evaluating_operators() {
    let mut plan = expression("'12'::integer / 0");
    assert_eq!(
        analyze_default_inputs(
            &NoRoutines,
            &|_: &str| false,
            &Aliases,
            &mut plan,
            &assignment_context()
        )
        .unwrap(),
        Some(ColumnType::Integer)
    );
    let mut converted = false;
    plan.scalar.visit(&mut |node| {
        converted |= matches!(node, ScalarExpr::TypedLiteral { value: uqa_core::Value::Int(12), ty, .. } if ty == "pg_catalog.\"int4\"");
    });
    assert!(converted, "the input function's value must be retained");
}

struct RelationAliases(std::cell::RefCell<Vec<String>>);

impl crate::schema::dependencies::oid_alias::OidAliasInput for RelationAliases {
    fn resolve_oid_alias_input(
        &self,
        ty: &ColumnType,
        name: &str,
    ) -> Result<Option<i64>, SQLError> {
        assert_eq!(*ty, ColumnType::Regclass);
        self.0.borrow_mut().push(name.into());
        Ok((name == "ids").then_some(16_500))
    }
}

#[test]
fn default_sequence_inputs_store_relation_identity_without_sequence_evaluation() {
    let aliases = RelationAliases(std::cell::RefCell::new(Vec::new()));
    for text in ["nextval('ids')", "currval('ids')", "setval('ids',40)"] {
        let mut plan = expression(text);
        assert_eq!(
            analyze_default_inputs(
                &NoRoutines,
                &|_: &str| false,
                &aliases,
                &mut plan,
                &assignment_context(),
            )
            .unwrap(),
            Some(ColumnType::BigInteger)
        );
        let ScalarExpr::Func { args, .. } = plan.scalar else {
            panic!("sequence call must remain unevaluated");
        };
        assert!(matches!(
            &args[0],
            ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Int(16_500),
                bound_type: Some(ColumnType::Regclass),
                ..
            }
        ));
    }
    assert_eq!(*aliases.0.borrow(), ["ids", "ids", "ids"]);
    let error = analyze_default_inputs(
        &NoRoutines,
        &|_: &str| false,
        &aliases,
        &mut expression("nextval('missing')"),
        &assignment_context(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P01"));
    assert_eq!(error.to_string(), "relation \"missing\" does not exist");
}

#[test]
fn stored_default_function_coercions_preserve_late_text_and_bigint_arguments() {
    for (text, expected_type) in [
        ("nextval('missing'::text)", ColumnType::Regclass),
        ("setval('ids',40)", ColumnType::BigInteger),
    ] {
        let aliases = RelationAliases(std::cell::RefCell::new(Vec::new()));
        let original = expression(text);
        let mut bound = original.clone();
        let binding = assignment_context();
        analyze_default_inputs(
            &NoRoutines,
            &|_: &str| false,
            &aliases,
            &mut bound,
            &binding,
        )
        .unwrap();
        crate::binding::bind_expression_plan_routines_for_storage(
            &NoRoutines,
            &mut bound,
            &[],
            &binding,
            &RowSchema::default(),
        )
        .unwrap();
        let ScalarExpr::Func { args, .. } = &bound.scalar else {
            panic!("stored sequence call");
        };
        let argument = args.last().unwrap();
        assert!(
            matches!(argument, ScalarExpr::Cast { ty, .. } if ty == &expected_type.catalog_name())
        );
        if matches!(expected_type, ColumnType::Regclass) {
            assert!(
                aliases.0.borrow().is_empty(),
                "typed text resolves at runtime"
            );
        }
        crate::binding::syntax_sites::expression_syntax_sites(&original, &bound).unwrap();
    }
}

#[test]
fn stored_function_coercions_keep_named_argument_positions() {
    let mut plan = expression("random(max => 2::bigint, min => 1)");
    let binding = assignment_context();
    let analyze = |plan: &mut ExpressionPlan| {
        analyze_default_inputs(&NoRoutines, &|_: &str| false, &Aliases, plan, &binding)
    };
    assert_eq!(analyze(&mut plan).unwrap(), Some(ColumnType::BigInteger));
    crate::binding::bind_expression_plan_routines_for_storage(
        &NoRoutines,
        &mut plan,
        &[],
        &binding,
        &RowSchema::default(),
    )
    .unwrap();
    assert_eq!(analyze(&mut plan).unwrap(), Some(ColumnType::BigInteger));
    let ScalarExpr::Func { args, .. } = &plan.scalar else {
        panic!("random call")
    };
    let arguments = crate::scalar_call_arguments(args).unwrap();
    assert_eq!(arguments[0].name, Some("max"));
    assert_eq!(arguments[1].name, Some("min"));
    assert!(
        matches!(arguments[1].value, ScalarExpr::Cast { expr, ty, .. } if ty == &ColumnType::BigInteger.catalog_name() && matches!(expr.as_ref(), ScalarExpr::Literal(uqa_core::Value::Int(1))))
    );
}

#[test]
fn default_membership_inputs_finish_before_enclosing_call_replacement() {
    for (sql, expected) in [
        ("pg_typeof(1 IN (NULL, NULL))", ColumnType::Regtype),
        ("coalesce(1 NOT IN ('1', NULL), false)", ColumnType::Boolean),
    ] {
        let mut plan = expression(sql);
        let binding = assignment_context();
        for _ in 0..2 {
            assert_eq!(
                analyze_default_inputs(
                    &NoRoutines,
                    &|_: &str| false,
                    &Aliases,
                    &mut plan,
                    &binding
                )
                .unwrap_or_else(|error| panic!("{sql}: {error}")),
                Some(expected.clone()),
            );
            plan.scalar
                .visit(&mut |node| assert!(!matches!(node, ScalarExpr::InList { .. })));
        }
    }
}

fn observe_stored_scalar_binding(
    text: &str,
    steps: usize,
    cancel: bool,
) -> (
    Result<(crate::ast::Expr, Option<ColumnType>), SQLError>,
    usize,
) {
    use crate::binding::{
        bind_expression_plan_routines_for_storage_with_work, stored_routines::StoredCallOrigin,
        syntax_sites::expression_syntax_sites_with_work,
    };
    use uqa_core::{
        image_io::{ImageDecodeWindow, ImageDecoder},
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let crate::Statement::CreateFunction(mut definition) = crate::compile(&format!(
        "CREATE FUNCTION f(v integer DEFAULT {text}) RETURNS integer LANGUAGE sql AS 'SELECT v'"
    ))
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let mut syntax = definition.params[0].default.take().unwrap();
    let mut ordinary_syntax = syntax.clone();
    let original = ExpressionPlan::lower(syntax.clone());
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let original_token = CancellationToken::new();
    let mut decoder = ImageDecoder::new(&memory, &token, move |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, &original_token, input.cancellation);
        let expression = original.copy_for_rewrite(&control)?;
        let context = assignment_context();
        let (bound, ty) = bind_expression_plan_routines_for_storage_with_work(
            &NoRoutines,
            expression,
            &[],
            context.copy_with_control(&control)?,
            &RowSchema::default(),
            &control,
            &mut input,
        )
        .await?;
        let mut ordinary = original.clone();
        let ordinary_ty = crate::binding::bind_expression_plan_routines_for_storage(
            &NoRoutines,
            &mut ordinary,
            &[],
            &context,
            &RowSchema::default(),
        )?;
        assert_eq!(
            bound.scalar, ordinary.scalar,
            "retained routine input and ordinary binding"
        );
        let ordinary_sites =
            crate::binding::syntax_sites::expression_syntax_sites(&original, &ordinary)?;
        let ordinary_changed = StoredCallOrigin::Persisted
            .bind_expression_sites(&mut ordinary_syntax, &ordinary_sites)?;
        let sites =
            expression_syntax_sites_with_work(&original, &bound, &control, &mut input).await?;
        let edited = StoredCallOrigin::Persisted
            .bind_expression_sites_with_work(&mut syntax, &sites, &control, &mut input)
            .await?;
        assert_eq!(((*edited), &syntax), (ordinary_changed, &ordinary_syntax));
        let ty = ty.map(|ty| ty.into_parts().0);
        assert_eq!(ty, ordinary_ty);
        Ok((syntax, ty))
    })
    .unwrap();
    let mut calls = 0;
    let result = loop {
        calls += 1;
        if cancel && calls == 8 {
            token.cancel();
        }
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(Some(result)) => break Ok(result),
            Ok(None) => (),
            Err(error) => break Err(error),
        }
    };
    drop(decoder);
    assert_eq!(memory.used(), 0);
    (result, calls)
}

#[test]
fn stored_scalar_analysis_pairs_the_original_syntax_in_admitted_work_windows() {
    for text in [
        "abs(-1)",
        "random(max => 2::bigint, min => 1)",
        "ARRAY[40::bigint, 2]",
    ] {
        observe_stored_scalar_binding(text, 1, false)
            .0
            .unwrap_or_else(|error| panic!("{text}: {error}"));
    }
    let small = observe_stored_scalar_binding("ARRAY[1]", 1, false);
    small.0.unwrap();
    let wide = format!("ARRAY[{}]", vec!["1"; 4096].join(","));
    let single = observe_stored_scalar_binding(&wide, 1, false);
    single.0.unwrap();
    let batched = observe_stored_scalar_binding(&wide, 32, false);
    batched.0.unwrap();
    assert!(single.1 > small.1 + 4096 && batched.1 < single.1 / 8);
    let cancelled = observe_stored_scalar_binding(&wide, 1, true);
    assert_eq!(cancelled.0.unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(cancelled.1, 8);
    let error = observe_stored_scalar_binding("ARRAY[missing, $1]", 1, false)
        .0
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
}

#[test]
fn wide_routine_defaults_keep_the_first_query_window_or_name_error() {
    let row = std::iter::repeat_n("(1)", 1024)
        .collect::<Vec<_>>()
        .join(",");
    let order = std::iter::repeat_n("1", 1024).collect::<Vec<_>>().join(",");
    let query = format!("(SELECT v FROM (VALUES {row}) AS s(v))");
    let window = format!("row_number() OVER (ORDER BY {order})");
    for (first, last, state, diagnostic) in [
        (
            query.as_str(),
            "absent",
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ),
        (
            window.as_str(),
            "absent",
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ),
        (
            "absent",
            query.as_str(),
            "42703",
            "column \"absent\" does not exist",
        ),
        (
            "absent",
            window.as_str(),
            "42703",
            "column \"absent\" does not exist",
        ),
    ] {
        let crate::Statement::CreateFunction(definition) = crate::compile(&format!(
            "CREATE FUNCTION f(v bigint[] DEFAULT ARRAY[{first},{last}]) RETURNS bigint[] LANGUAGE sql AS 'SELECT v'"
        )).unwrap().remove(0) else { unreachable!() };
        let original = crate::routines::SQLUserFunction::new(
            *definition,
            crate::routines::RoutineBody::Source,
        )
        .shared();
        let token = uqa_core::CancellationToken::new();
        let memory = uqa_core::memory::MemoryBudget::new(16 << 20);
        let control = uqa_core::memory::ProductionControl::new(&memory, &token, &token);
        let mut capture =
            crate::routines::StoredRoutineDefinition::new(original, &control).unwrap();
        while !capture.copy_next_field(&control).unwrap() {}
        let (definition, _) = capture.into_definition(&control).unwrap();
        let mut plan = ExpressionPlan::lower(definition.params[0].default.clone().unwrap());
        let error = analyze_default_inputs(
            &NoRoutines,
            &|_: &str| false,
            &Aliases,
            &mut plan,
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string()),
            (Some(state), diagnostic.to_owned())
        );
        drop(definition);
        assert_eq!(memory.used(), 0);
    }
}
