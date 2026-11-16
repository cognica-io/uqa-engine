//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::FunctionBinding, SQLError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

type Input = ImageDecoder<Produced<FunctionBinding>, SQLError>;

fn input(
    source: Arc<FunctionBinding>,
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
        source.clone_from_input(&control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Produced<FunctionBinding>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Produced<FunctionBinding>, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("stored call copy stopped progressing")
}

fn binding() -> FunctionBinding {
    serde_json::from_value(serde_json::json!({
        "name": "s.call_型",
        "object_id": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],
        "argument_types": ["integer", "text[]"],
        "builtin": false,
        "dispatch": {"ArraySlicesWithBounds": {"omitted_lower": 255, "omitted_upper": 128}},
        "invocation": {
            "argument_positions": [1,0,1],
            "argument_targets": ["text", "integer", "text"],
            "argument_sources": [null, "smallint", "text"],
            "parameter_types": ["integer", "text[]"],
            "return_type": "record",
            "variadic_mode": {"Expanded": {"parameter_index": 1}}
        },
        "composite_field": {
            "type_oid": 17000, "number": -3, "result_type": "Text",
            "collation_oid": 950, "dropped": true,
            "changed_type": {"Array": {"Domain": {
                "schema": "s", "name": "type_型", "oid": 19000,
                "array_oid": 19001, "collation_oid": 951, "base": "Text"
            }}}
        },
        "resolution_error": {"Operator": {"sqlstate": "42883", "message": "missing overload"}},
        "input_collation": 950
    }))
    .unwrap()
}

#[test]
fn retained_call_copy_preserves_bound_identity_invocation_types_and_exact_payload_ownership() {
    let mut source = binding();
    source.name = "call_型🦀\"".repeat(2048);
    source.argument_types.push("schema.type_é".repeat(1024));
    for variadic in [
        crate::ast::RoutineVariadicMode::None,
        crate::ast::RoutineVariadicMode::Expanded { parameter_index: 1 },
        crate::ast::RoutineVariadicMode::Explicit { parameter_index: 1 },
    ] {
        source.invocation.as_mut().unwrap().variadic_mode = variadic;
        let source = Arc::new(source.clone());
        let expected = serde_json::to_value(&*source).unwrap();
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let ordinary = source.clone_with_control(&control).unwrap();
        let weak = Arc::downgrade(&source);
        let mut reader = input(source, &native, &work, &token, &token);
        let (value, polls) = complete(&mut reader);
        assert!(polls > 40, "{polls}");
        assert!(weak.upgrade().is_none());
        assert_eq!(serde_json::to_value(&*value).unwrap(), expected);
        assert_eq!(*value, *ordinary);
        assert_eq!(value.reserved_bytes(), ordinary.reserved_bytes());
        assert_eq!(native.used(), value.reserved_bytes() * 2);
        assert_eq!(work.used(), 0);
        drop((value, ordinary));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_call_copy_retires_every_suspended_prefix_and_both_cancellation_owners() {
    let source = Arc::new(binding());
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(19).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("XX001"));
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (19, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_call_copy_bounds_wide_invocations_and_admits_destination_before_copying() {
    let mut source = binding();
    let invocation = source.invocation.as_mut().unwrap();
    invocation.argument_positions = (0..2048).collect();
    invocation.argument_targets = vec!["text".into(); 2048];
    invocation.argument_sources = vec![Some("name".into()); 2048];
    invocation.parameter_types = vec!["text".into(); 2048];
    let source = Arc::new(source);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert_eq!(*value, *source);
    assert!(polls > 8192, "{polls}");
    drop(value);
    for bytes in [0, 1, 64, 8192, 32768, 65536] {
        let native = MemoryBudget::new(bytes);
        let error = input(source.clone(), &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
        assert!(native.peak() <= bytes);
    }
}

#[test]
fn retained_call_copy_preserves_assignment_paths_and_both_resolution_error_variants() {
    let mut source = binding();
    source.value_binding = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "assignment": {
                "path": [{"Field": {"type_oid": 1234, "number": -3}},
                         {"Subscripts": {"first_argument": 2, "dimensions": 6, "slice": true,
                             "bounds": {"omitted_lower": 255, "omitted_upper": 192}}}],
                "value_argument": 4
            }
        }))
        .unwrap(),
    ));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for error in [
        None,
        Some(Box::new(
            crate::ast::FunctionResolutionError::UndefinedFunction {
                signature: "s.function(integer)".repeat(1024),
            },
        )),
        source.resolution_error.clone(),
    ] {
        source.resolution_error = error;
        let (value, _) = complete(&mut input(
            Arc::new(source.clone()),
            &native,
            &work,
            &token,
            &token,
        ));
        assert_eq!(*value, source);
        drop(value);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
