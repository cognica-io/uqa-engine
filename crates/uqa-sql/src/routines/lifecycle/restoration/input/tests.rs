//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBody, FunctionParamMode};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError};

type Groups = Vec<(String, bool, Vec<CreateFunction>)>;
type NativeGroups = Vec<(String, bool, Vec<Produced<CreateFunction>>)>;
type Decoder = ImageDecoder<CanonicalRoutineDefinitions, RoutineRestoreInputError>;

fn definition(name: &str, id: u8, types: &[&str]) -> CreateFunction {
    let crate::Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION f(x integer DEFAULT 1) RETURNS integer AS 'this body is compiled only when invoked' LANGUAGE SQL"
    ).unwrap().remove(0) else { unreachable!() };
    let parameter = definition.params[0].clone();
    definition.params = types
        .iter()
        .map(|name| {
            let mut parameter = parameter.clone();
            parameter.type_name = (*name).into();
            parameter
        })
        .collect();
    definition.name = name.into();
    definition.object_id = Some([id; 16]);
    *definition
}

fn decoder(
    groups: &Groups,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let control = ProductionControl::new(native, original, invoking);
    let groups = groups
        .iter()
        .map(|(name, exists, values)| {
            (
                name.clone(),
                *exists,
                values
                    .iter()
                    .map(|value| {
                        crate::plan::copy::capture_stored_with_control(value, &control).unwrap()
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    native_decoder(groups, native, work, original, invoking)
}

fn native_decoder(
    groups: NativeGroups,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut builder = RetainedRoutineRestoreBuilder::new(&native);
        for (name, exists, values) in groups {
            let relation =
                persisted_registry_relation_from_input(&name, &original, &control, &mut input)
                    .await?;
            validate_persisted_routine_schema_from_input(
                &name, &relation, exists, &control, &mut input,
            )
            .await?;
            for value in values {
                builder = builder
                    .insert_from_input(&name, &relation, value, &original, &mut input)
                    .await?;
            }
        }
        builder.finish_from_input(&original, &mut input).await
    })
    .unwrap()
}

#[test]
fn retained_routine_canonicalization_transfers_original_parameter_and_body_allocations() {
    for statements in [false, true] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let mut original = definition("f", 1, &["integer", "varchar(9)"]);
        if statements {
            original.body = FunctionBody::Statements(crate::compile("SELECT 1, 2").unwrap());
        }
        let value = crate::plan::copy::capture_stored_with_control(&original, &control).unwrap();
        let parameters = value.params.as_ptr();
        let argument = value.params[0].type_name.as_ptr();
        let body = match &value.body {
            FunctionBody::Source(text) => text.as_ptr() as usize,
            FunctionBody::Statements(values) => values.as_ptr() as usize,
        };
        let original_bytes = value.reserved_bytes();
        let old_name_bytes = value.name.capacity();
        let groups = vec![("f".into(), true, vec![value])];
        let (output, _) = complete(
            &mut native_decoder(groups, &native, &work, &token, &token),
            1,
        )
        .unwrap();
        let (_, values) = output.definitions().next().unwrap();
        let value = &values[0];
        assert_eq!(value.name, "public.f");
        assert_eq!(value.params.as_ptr(), parameters);
        assert_eq!(value.params[0].type_name.as_ptr(), argument);
        assert_eq!(
            match &value.body {
                FunctionBody::Source(text) => text.as_ptr() as usize,
                FunctionBody::Statements(values) => values.as_ptr() as usize,
            },
            body
        );
        assert_eq!(
            value.reserved_bytes(),
            original_bytes - old_name_bytes + value.name.capacity()
        );
        drop(output);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("canonicalization only consumes prepared native declarations")
    }
}
fn advance(
    input: &mut Decoder,
    steps: usize,
) -> Result<Option<CanonicalRoutineDefinitions>, RoutineRestoreInputError> {
    input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
}
fn complete(
    input: &mut Decoder,
    steps: usize,
) -> Result<(CanonicalRoutineDefinitions, usize), RoutineRestoreInputError> {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, polls));
        }
    }
    panic!("routine canonicalization stopped progressing")
}

fn ordinary(groups: &Groups) -> Result<serde_json::Value, String> {
    let mut builder = super::super::RoutineRestoreBuilder::default();
    let mut migrated = false;
    for (name, exists, definitions) in groups {
        let relation = super::super::persisted_registry_relation(name)?;
        super::super::validate_persisted_routine_schema(name, &relation, *exists)?;
        for value in definitions {
            migrated |= builder.insert(name, &relation, value.clone())?;
        }
    }
    if migrated {
        return Err("routine catalog requires an initial-open expression, object-identity or ACL format migration".into());
    }
    Ok(serde_json::to_value(builder.into_definitions()).unwrap())
}

fn observed(value: &CanonicalRoutineDefinitions) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    for (name, definitions) in value.definitions() {
        result.insert(
            name.into(),
            serde_json::Value::Array(
                definitions
                    .iter()
                    .map(|value| serde_json::to_value(&**value).unwrap())
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(result)
}

#[test]
fn retained_routine_canonicalization_preserves_alias_groups_overload_order_and_native_owners() {
    let mut procedure = definition("public.f", 2, &["numeric(8,2)"]);
    procedure.is_procedure = true;
    procedure.params[0].mode = FunctionParamMode::InOut;
    let groups = vec![
        (
            "\"A.B\".\"f\"\"x\"".into(),
            true,
            vec![definition(
                "\"A.B\".\"f\"\"x\"",
                3,
                &["\"named\"", "boolean[]"],
            )],
        ),
        ("empty".into(), true, vec![]),
        (
            "f".into(),
            true,
            vec![definition("public.f", 1, &["PG_CATALOG.INTEGER"])],
        ),
        ("public.f".into(), true, vec![procedure]),
    ];
    let expected = ordinary(&groups).unwrap();
    for steps in [1, 11, 256] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(&groups, &native, &work, &token, &token);
        let (output, _) = complete(&mut input, steps).unwrap();
        assert_eq!(observed(&output), expected);
        assert!(advance(&mut input, steps).is_err());
        assert_eq!(work.used(), 0);
        let owned = output
            .definitions
            .iter()
            .map(|(name, group)| {
                name.capacity()
                    + OwnedMap::<String, Group>::entry_bytes()
                    + group.definitions.capacity() * size_of::<Produced<CreateFunction>>()
                    + group
                        .definitions
                        .iter()
                        .map(Produced::reserved_bytes)
                        .sum::<usize>()
            })
            .sum::<usize>();
        assert_eq!(native.used(), owned);
        token.cancel();
        assert_eq!(observed(&output), expected);
        drop((input, output));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_routine_canonicalization_preserves_original_diagnostic_and_migration_precedence() {
    let mut missing = definition("f", 1, &["int4"]);
    missing.object_id = None;
    let mut legacy = definition("f", 1, &["int4"]);
    let crate::Statement::Select(select) =
        crate::compile("SELECT __to_hex_int4(1)").unwrap().remove(0)
    else {
        unreachable!()
    };
    legacy.params[0].default = Some(select.projections[0].expr.clone());
    let cases = [
        vec![(String::new(), false, vec![])],
        vec![("\"missing".into(), false, vec![])],
        vec![("unknown.f".into(), false, vec![])],
        vec![("f".into(), true, vec![missing])],
        vec![(
            "f".into(),
            true,
            vec![definition("f", 1, &[]), definition("bad\"name", 1, &[])],
        )],
        vec![("f".into(), true, vec![definition("bad\"name", 1, &[])])],
        vec![("f".into(), true, vec![definition("other.f", 1, &[])])],
        vec![(
            "f".into(),
            true,
            vec![
                definition("f", 1, &["integer", "boolean[]"]),
                definition("f", 2, &["int4", "bool[][]"]),
            ],
        )],
        vec![("f".into(), true, vec![legacy.clone()])],
        vec![(
            "f".into(),
            true,
            vec![legacy, definition("other.f", 2, &[])],
        )],
    ];
    for groups in cases {
        let expected = ordinary(&groups).unwrap_err();
        for steps in [1, 31] {
            let native = MemoryBudget::new(1 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = decoder(&groups, &native, &work, &token, &token);
            let error = complete(&mut input, steps)
                .err()
                .expect("original rejection");
            assert_eq!(error.to_string(), expected);
            assert!(matches!(&error, RoutineRestoreInputError::Diagnostic(_)));
            assert_eq!(work.used(), 0);
            assert!(
                native.used() > 0,
                "diagnostic owns its payload after candidate retirement"
            );
            assert!(advance(&mut input, steps).is_err());
            drop((error, input));
            assert_eq!(native.used(), 0);
        }
    }
}

#[test]
fn retained_routine_canonicalization_bounds_wide_names_signatures_and_both_cancellation_owners() {
    let name = format!("public.\"{}\"", "a\"\"字".repeat(1024));
    let argument = "named_type".repeat(1024);
    let groups = vec![(
        name.clone(),
        true,
        vec![
            definition(&name, 1, &[&argument]),
            definition(&name, 2, &["text"]),
        ],
    )];
    let native = MemoryBudget::new(1 << 24);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) =
        complete(&mut decoder(&groups, &native, &work, &token, &token), 1).unwrap();
    assert_eq!(observed(&output), ordinary(&groups).unwrap());
    assert!(polls > 100);
    drop(output);
    assert_eq!((native.used(), work.used()), (0, 0));
    let small = vec![(
        "f".into(),
        true,
        vec![
            definition("f", 1, &["integer"]),
            definition("f", 2, &["text"]),
        ],
    )];
    let (output, polls) =
        complete(&mut decoder(&small, &native, &work, &token, &token), 1).unwrap();
    drop(output);
    for stop in 0..polls {
        for cancelled in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(&small, &native, &work, &original, &invoking);
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            if let Some(original_owner) = cancelled {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                let error = advance(&mut input, 1).err().expect("cancelled input");
                assert!(matches!(
                    error,
                    RoutineRestoreInputError::Work(ImageIoError::Cancelled(_))
                        | RoutineRestoreInputError::Native(
                            uqa_core::ValueRetentionError::Cancelled(_)
                        )
                ));
                assert_eq!((native.used(), work.used()), (0, 0));
                assert!(advance(&mut input, 1).is_err());
            }
            drop(input);
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_routine_canonicalization_retires_native_sources_on_admission_failure() {
    let groups = vec![(
        "public.f".into(),
        true,
        vec![definition("public.f", 1, &["text"])],
    )];
    for headroom in [0, 8, 32] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(&groups, &native, &work, &token, &token);
        let occupied = native.reserve(native.available() - headroom).unwrap();
        let error = complete(&mut input, 1)
            .err()
            .expect("native admission denied");
        assert!(matches!(
            error,
            RoutineRestoreInputError::Native(uqa_core::ValueRetentionError::Memory(_))
        ));
        assert_eq!(native.used(), occupied.bytes());
        assert_eq!(work.used(), 0);
        assert!(advance(&mut input, 1).is_err());
        drop((input, occupied));
        assert_eq!(native.used(), 0);
    }
}
