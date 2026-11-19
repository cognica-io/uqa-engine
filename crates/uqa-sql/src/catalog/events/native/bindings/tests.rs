//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{InternalRelationId, Statement},
    binding::fixture::{empty_binding_context, NoRoutines},
    catalog::{
        events::RuleConditionBinding,
        type_definitions::TypeDefinitions,
        type_references::{TypeReferenceInput, TypeReferences},
    },
    plan::ExpressionPlan,
    routines::native::NativeRoutineReferences,
    ColumnType,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn rule(condition: &str) -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile(&format!(
        "CREATE RULE saved AS ON UPDATE TO public.items WHERE {condition} DO NOTHING"
    ))
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    let plan = ExpressionPlan::lower(definition.condition.clone().unwrap());
    StoredRule {
        definition,
        enabled: crate::ast::EventEnableMode::default(),
        condition_plan: Some(plan),
        condition_binding: Some(RuleConditionBinding::from_captured_parts(
            Some(InternalRelationId::from_raw(101)),
            Some(InternalRelationId::from_raw(202)),
            vec!["removed".into(), "id".into(), "flag".into()],
        )),
        dependencies: None,
        catalog_oid: Some(31_000),
    }
}

fn validate(rule: &StoredRule, control: &ProductionControl<'_>) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate_with_work(rule, control, &mut ()))
}

async fn validate_with_work<W: crate::catalog::CatalogWork + Send>(
    rule: &StoredRule,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    let memory = control.budget().unwrap();
    let token = CancellationToken::new();
    let binding = empty_binding_context().copy_with_control(control)?;
    let references = NativeRoutineReferences::new(std::iter::empty(), memory, &token)?;
    let input = TypeReferenceInput {
        domains: &TypeDefinitions::default(),
        enums: &TypeDefinitions::default(),
        composites: &TypeDefinitions::default(),
        ranges: &TypeDefinitions::default(),
        shells: &TypeDefinitions::default(),
    };
    let types = TypeReferences::new(input, std::iter::empty(), memory, &token)?;
    let context = NativeScalarContext {
        subject: "rule condition",
        routines: &NoRoutines,
        references: &references,
        types: &types,
        binding: &binding,
    };
    let row = RowSchema::with_column_metadata_with_control(
        None,
        [
            ("flag", Some(&ColumnType::Boolean), None),
            ("id", Some(&ColumnType::Integer), None),
            ("added", Some(&ColumnType::Text), None),
        ],
        control,
    )?;
    validate_condition_with_work(rule, &row, &context, control, work).await
}

#[test]
fn native_rule_condition_types_keep_captured_positions_and_nested_boolean_scopes() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for condition in [
        "old.flag",
        "old.id > 0",
        "NULL",
        "'true' COLLATE \"C\"",
        "1 / 0 = 0",
        "EXISTS(SELECT 1 WHERE old.flag)",
        "EXISTS(WITH x AS (SELECT old.flag AS flag) SELECT 1 FROM x WHERE flag)",
        "(SELECT old.flag)",
    ] {
        let rule = rule(condition);
        let before = serde_json::to_vec(&rule).unwrap();
        validate(&rule, &control).unwrap_or_else(|error| panic!("{condition}: {error}"));
        assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
        assert_eq!(memory.used(), 0, "{condition}");
    }
    let mut rule = rule("old.flag");
    rule.condition_plan.as_mut().unwrap().scalar =
        ScalarExpr::InternalColumn(InternalRelationId::from_raw(101).column(2));
    let before = serde_json::to_vec(&rule).unwrap();
    validate(&rule, &control).unwrap();
    assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_rule_condition_types_reject_invalid_scalar_and_nested_inputs() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for condition in [
        "42",
        "old.id",
        "true AND old.id",
        "'invalid_boolean'",
        "$1",
        "missing.flag",
        "sum(old.id)>0",
        "unnest(ARRAY[old.flag])",
        "row_number() OVER () > 0",
        "EXISTS(SELECT 1 WHERE old.id)",
        "EXISTS(SELECT 1 WHERE $1)",
        "(SELECT old.id)",
    ] {
        let rule = rule(condition);
        let before = serde_json::to_vec(&rule).unwrap();
        assert!(validate(&rule, &control).is_err(), "{condition}");
        assert_eq!(serde_json::to_vec(&rule).unwrap(), before);
        assert_eq!(memory.used(), 0, "{condition}");
    }
}

#[test]
fn native_rule_condition_type_analysis_retires_denied_and_cancelled_scratch() {
    let rule = rule("EXISTS(SELECT 1 WHERE old.flag)");
    let mut successes = 0;
    let mut denials = 0;
    for limit in [0, 32, 512, 4096, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        match validate(&rule, &control) {
            Ok(()) => successes += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                denials += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(successes > 0 && denials > 0);
    for original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let source = CancellationToken::new();
        let caller = CancellationToken::new();
        if original {
            source.cancel();
        } else {
            caller.cancel();
        }
        let control = ProductionControl::new(&memory, &source, &caller);
        assert!(validate(&rule, &control).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn captured_rule_conditions_yield_inside_a_values_subquery_before_a_late_unknown_column() {
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

    const WIDTH: usize = 256;
    let run = |invalid: Option<usize>, stop: Option<usize>| {
        let values = (0..WIDTH)
            .map(|position| {
                if invalid == Some(position) {
                    "(missing)"
                } else {
                    "(1)"
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let rule = rule(&format!("EXISTS(VALUES {values})"));
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let original = token.clone();
        let expected = validate(&rule, &ProductionControl::new(&memory, &token, &token))
            .map_err(|error| error.to_string());
        assert_eq!(memory.used(), 0);
        let mut decoder = ImageDecoder::new(&memory, &token, move |mut channel| async move {
            let mut input = channel.input();
            let control = ProductionControl::new(input.memory, &original, input.cancellation);
            validate_with_work(&rule, &control, &mut input).await
        })
        .unwrap();
        let mut calls = 0;
        let result = loop {
            if stop == Some(calls) {
                token.cancel();
            }
            calls += 1;
            match decoder.advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            ) {
                Ok(None) => (),
                Ok(Some(())) => break Ok(()),
                Err(error) => break Err(error),
            }
        };
        if stop.is_some() {
            assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        } else {
            if invalid.is_some() {
                assert!(matches!(result, Err(ref error) if error.sqlstate() == Some("42703")));
            }
            assert_eq!(result.map_err(|error| error.to_string()), expected);
        }
        drop(decoder);
        assert_eq!(memory.used(), 0);
        calls
    };
    let first = run(Some(0), None);
    let last = run(Some(WIDTH - 1), None);
    assert!(last >= first + WIDTH - 1, "first {first}, last {last}");
    run(Some(WIDTH - 1), Some(first + WIDTH / 2));
    run(None, None);
}
