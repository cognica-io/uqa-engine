//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<(), SQLError>;

fn declaration(sql: &str) -> CreateFunction {
    let crate::ast::Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0)
    else {
        panic!("expected routine declaration")
    };
    *definition
}

fn input(
    definition: Arc<CreateFunction>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        validate_routine_declaration_from_input(&definition, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> Result<usize, SQLError> {
    for polls in 1..100_000 {
        if advance(input)?.is_some() {
            return Ok(polls);
        }
    }
    panic!("routine declaration stopped progressing")
}

fn outcome(value: Result<(), SQLError>) -> serde_json::Value {
    match value {
        Ok(()) => serde_json::json!({"sqlstate": null, "message": null, "detail": null}),
        Err(error) => {
            let detail = match error.primary() {
                SQLError::Diagnostic { detail, .. } => detail.as_deref(),
                _ => None,
            };
            serde_json::json!({"sqlstate": error.sqlstate(), "message": error.to_string(), "detail": detail})
        }
    }
}

#[test]
fn retained_routine_declaration_preserves_postgresql_pseudo_types_polymorphic_families_and_error_order(
) {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/pg18.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 36);
    let mut differences = Vec::new();
    for case in cases {
        let definition = Arc::new(declaration(case["sql"].as_str().unwrap()));
        let expected = serde_json::json!({"sqlstate": case["sqlstate"], "message": case["message"], "detail": case["detail"]});
        let ordinary = outcome(validate_inline(&definition));
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let retained =
            outcome(input(definition, &native, &work, &token, &token).read(&mut std::io::empty()));
        if ordinary != expected || retained != expected {
            differences.push(serde_json::json!({"id": case["id"], "expected": expected, "ordinary": ordinary, "retained": retained}));
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    assert!(differences.is_empty(), "{differences:#?}");
}

#[test]
fn retained_routine_declaration_bounds_type_names_and_all_parameter_visits_without_reading_source_bodies(
) {
    let base =
        declaration("CREATE FUNCTION f(a integer) RETURNS integer LANGUAGE sql AS 'SELECT a'");
    let mut wide = base.clone();
    wide.params[0].type_name = "quoted_型".repeat(8192);
    wide.body = FunctionBody::Source("uncompiled source body".repeat(8192));
    let mut many = base;
    many.params = (0..2048)
        .map(|index| {
            let mut parameter = many.params[0].clone();
            parameter.name = format!("p_{index}");
            parameter
        })
        .collect();
    for (definition, minimum_polls) in [(wide, 100), (many, 4096)] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let definition = Arc::new(definition);
        validate_inline(&definition).unwrap();
        let original_name = definition.params[0].type_name.as_ptr();
        let polls = complete(&mut input(
            definition.clone(),
            &native,
            &work,
            &token,
            &token,
        ))
        .unwrap();
        assert!(polls >= minimum_polls, "{polls} < {minimum_polls}");
        assert_eq!(definition.params[0].type_name.as_ptr(), original_name);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_routine_declaration_retires_every_suspended_prefix_and_both_cancellation_owners() {
    let mut definition = declaration(
        "CREATE FUNCTION f(a integer, b text, c integer) RETURNS integer LANGUAGE sql AS 'SELECT a'",
    );
    definition.params[1].type_name = "type_型".repeat(1024);
    let definition = Arc::new(definition);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let polls = complete(&mut input(
        definition.clone(),
        &native,
        &work,
        &token,
        &token,
    ))
    .unwrap();
    let sibling = native.reserve(13).unwrap();
    for prefix in 0..polls - 1 {
        for cancelled in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(definition.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut input).unwrap().is_none());
            }
            if cancelled != 0 {
                if cancelled == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                let error = advance(&mut input).unwrap_err();
                assert_eq!(error.sqlstate(), Some("57014"));
                assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("XX001"));
            }
            drop(input);
            assert_eq!((native.used(), work.used()), (13, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_routine_declaration_rejects_unadmitted_names_and_preserves_empty_signatures() {
    let mut wide =
        declaration("CREATE FUNCTION f(a integer) RETURNS integer LANGUAGE sql AS 'SELECT a'");
    wide.params[0].type_name = "wide_型".repeat(8192);
    let wide = Arc::new(wide);
    for limit in [0, 1, 64, 128, 1024] {
        let native = MemoryBudget::new(limit);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(wide.clone(), &native, &work, &token, &token);
        let mut failure = None;
        let allocations = allocation_counter::measure(|| {
            failure = Some(complete(&mut input).unwrap_err());
        });
        assert_eq!(failure.unwrap().sqlstate(), Some("53200"));
        assert!(allocations.bytes_max < 4096, "{allocations:?}");
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    let mut empty = declaration("CREATE PROCEDURE empty() LANGUAGE sql AS 'SELECT 1'");
    empty.body = FunctionBody::Statements(Vec::new());
    let native = MemoryBudget::new(0);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    complete(&mut input(Arc::new(empty), &native, &work, &token, &token)).unwrap();
    assert_eq!((native.used(), work.used()), (0, 0));
}
