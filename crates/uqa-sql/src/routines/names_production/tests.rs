//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionParam, FunctionParamMode},
    type_resolution::RoutineSignatureInputError,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken, ValueRetentionError,
};

type Decoder = ImageDecoder<Produced<Vec<String>>, RoutineSignatureInputError>;

fn declaration(
    parameters: impl IntoIterator<Item = (FunctionParamMode, String)>,
) -> CreateFunction {
    let crate::Statement::CreateFunction(mut definition) =
        crate::compile("CREATE FUNCTION public.f() RETURNS int RETURN 1")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    definition.params = parameters
        .into_iter()
        .map(|(mode, type_name)| FunctionParam {
            mode,
            type_name,
            name: String::new(),
            type_reference: None,
            written_type: None,
            default: None,
            default_type: None,
        })
        .collect();
    *definition
}

fn decoder(
    definition: Arc<CreateFunction>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Decoder, RoutineSignatureInputError> {
    let retained = memory.clone();
    let original = original.clone();
    Ok(ImageDecoder::new(
        memory,
        invoking,
        |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(&retained, &original, input.cancellation);
            routine_signature_types_from_input(&definition, &control, &mut input).await
        },
    )?)
}

fn advance(
    input: &mut Decoder,
    steps: usize,
) -> Result<Option<Produced<Vec<String>>>, RoutineSignatureInputError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn complete(
    input: &mut Decoder,
) -> Result<(Produced<Vec<String>>, usize), RoutineSignatureInputError> {
    for poll in 1..100_000 {
        if let Some(value) = advance(input, 1)? {
            return Ok((value, poll));
        }
    }
    panic!("native routine signature stopped progressing")
}

#[test]
fn retained_routine_signatures_preserve_declared_identity_order_for_functions_and_procedures() {
    use FunctionParamMode::{In, InOut, Out, Table, Variadic};
    for procedure in [false, true] {
        let mut definition = declaration([
            (Out, "text".into()),
            (In, "PG_CATALOG.INTEGER".into()),
            (Table, "numeric(4,2)".into()),
            (InOut, "public.\"持続 型\"".into()),
            (Variadic, "domain#20001[][]".into()),
        ]);
        definition.is_procedure = procedure;
        let expected = vec!["int4", "public.\"持続 型\"", "domain#20001[]"];
        assert_eq!(
            crate::routines::routine_signature_types(&definition),
            expected
        );
        let original = serde_json::to_value(&definition).unwrap();
        let definition = Arc::new(definition);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(Arc::clone(&definition), &memory, &token, &token).unwrap();
        let (value, _) = complete(&mut input).unwrap();
        assert_eq!(*value, expected);
        assert_eq!(serde_json::to_value(&*definition).unwrap(), original);
        drop(input);
        let owned = value.capacity() * size_of::<String>()
            + value.iter().map(String::capacity).sum::<usize>();
        assert_eq!(value.reserved_bytes(), owned);
        assert_eq!(memory.used(), owned);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_routine_signatures_bound_output_only_parameters_and_keep_empty_identity() {
    let definition = Arc::new(declaration(
        (0..4096).map(|_| (FunctionParamMode::Out, "integer".into())),
    ));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(definition, &memory, &token, &token).unwrap();
    let (value, polls) = complete(&mut input).unwrap();
    assert!(value.is_empty());
    assert!(polls >= 4096);
    assert_eq!(value.reserved_bytes(), 0);
    drop((value, input));
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_routine_signatures_retire_each_partial_parameter_and_both_cancellation_owners() {
    let definition = Arc::new(declaration([
        (FunctionParamMode::In, "integer".into()),
        (FunctionParamMode::Out, "text".into()),
        (
            FunctionParamMode::InOut,
            format!("\"{}\"(3)", "Á ".repeat(1024)),
        ),
        (
            FunctionParamMode::Variadic,
            "character varying(50)[][]".into(),
        ),
    ]));
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(Arc::clone(&definition), &memory, &token, &token).unwrap();
    let (value, polls) = complete(&mut input).unwrap();
    let peak = memory.peak();
    drop((value, input));
    assert_eq!(memory.used(), 0);
    for stop in 0..polls {
        for cancellation in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input =
                decoder(Arc::clone(&definition), &memory, &original, &invoking).unwrap();
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            if let Some(cancel_original) = cancellation {
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut input, 2),
                    Err(
                        RoutineSignatureInputError::Native(ValueRetentionError::Cancelled(_))
                            | RoutineSignatureInputError::Work(ImageIoError::Cancelled(_))
                    )
                ));
                assert_eq!(memory.used(), 0);
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [0, 512, peak / 2, peak - 1, peak] {
        let memory = MemoryBudget::new(limit);
        let result = decoder(Arc::clone(&definition), &memory, &token, &token)
            .and_then(|mut input| complete(&mut input).map(|(value, _)| value));
        match result {
            Ok(value) => {
                assert_eq!(
                    *value,
                    crate::routines::routine_signature_types(&definition)
                );
                assert_eq!(memory.used(), value.reserved_bytes());
                assert!(memory.peak() <= limit);
            }
            Err(
                RoutineSignatureInputError::Native(ValueRetentionError::Memory(_))
                | RoutineSignatureInputError::Work(ImageIoError::Memory(_)),
            ) => {
                assert!(limit < peak);
            }
            Err(error) => panic!("unexpected admission failure: {error}"),
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_empty_routine_signatures_observe_cancelled_input_admission() {
    let definition = declaration([]);
    let memory = MemoryBudget::new(1 << 20);
    let retained = memory.clone();
    let invoking = CancellationToken::new();
    let cancelled = invoking.clone();
    let original = CancellationToken::new();
    let mut input = ImageDecoder::new(&memory, &invoking, |mut reader| async move {
        let control = ProductionControl::new(&retained, &original, &original);
        let mut input = reader.input();
        cancelled.cancel();
        routine_signature_types_from_input(&definition, &control, &mut input).await
    })
    .unwrap();
    assert!(matches!(
        advance(&mut input, 1),
        Err(RoutineSignatureInputError::Work(ImageIoError::Cancelled(_)))
    ));
    assert_eq!(memory.used(), 0);
}
