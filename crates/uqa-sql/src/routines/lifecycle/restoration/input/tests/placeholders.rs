//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    registry::RoutineRegistry, routine_signature_types, RoutineBody, SQLUserFunction,
};
use std::sync::Arc;

type PlaceholderDecoder = ImageDecoder<RoutineRestorePlaceholders, RoutineRestoreInputError>;

fn canonical(
    groups: &Groups,
    native: &MemoryBudget,
    work: &MemoryBudget,
) -> CanonicalRoutineDefinitions {
    let token = CancellationToken::new();
    complete(&mut decoder(groups, native, work, &token, &token), 257)
        .unwrap()
        .0
}

fn placeholders(
    definitions: CanonicalRoutineDefinitions,
    previous: RoutineRegistry,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> PlaceholderDecoder {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        definitions
            .prepare_placeholders_from_input(&previous, &native, &original, &mut reader.input())
            .await
    })
    .unwrap()
}

fn finish(
    input: &mut PlaceholderDecoder,
    steps: usize,
) -> Result<(RoutineRestorePlaceholders, usize), RoutineRestoreInputError> {
    for polls in 1..1_000_000 {
        if let Some(output) =
            input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok((output, polls));
        }
    }
    panic!("routine placeholders stopped progressing")
}

fn fixture() -> Groups {
    let mut bound = definition("app.f", 2, &["bigint"]);
    bound.body = FunctionBody::Statements(crate::compile("SELECT 1").unwrap());
    vec![
        (
            "app.f".into(),
            true,
            vec![
                definition("app.f", 1, &["text"]),
                bound,
                definition("app.f", 3, &[]),
                definition("app.f", 4, &["int", "int"]),
                definition("app.f", 5, &["int"]),
            ],
        ),
        (
            "public.g".into(),
            true,
            vec![definition("g", 6, &["boolean"])],
        ),
    ]
}

fn previous_registry() -> RoutineRegistry {
    let mut previous = RoutineRegistry::new();
    let temporary =
        SQLUserFunction::new(definition("pg_temp_7.kept", 7, &[]), RoutineBody::Source).shared();
    previous
        .insert("pg_temp_7.kept".into(), vec![temporary])
        .unwrap();
    previous
        .insert(
            "public.removed".into(),
            vec![
                SQLUserFunction::new(definition("public.removed", 8, &[]), RoutineBody::Source)
                    .shared(),
            ],
        )
        .unwrap();
    previous
}

#[test]
fn retained_routine_placeholders_preserve_native_declarations_finalization_order_and_temporary_generations(
) {
    for steps in [1, 7, 257] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let previous = previous_registry();
        let selected_temporary = previous["pg_temp_7.kept"][0].clone();
        let definitions = canonical(&fixture(), &native, &work);
        let pointers = definitions
            .definitions()
            .flat_map(|(_, values)| {
                values.iter().map(|value| {
                    (
                        value.object_id.unwrap(),
                        value.params.as_ptr(),
                        value.name.as_ptr(),
                        match &value.body {
                            FunctionBody::Source(text) => text.as_ptr() as usize,
                            FunctionBody::Statements(statements) => statements.as_ptr() as usize,
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut input = placeholders(
            definitions,
            previous.clone(),
            &native,
            &work,
            &token,
            &token,
        );
        let (output, _) = finish(&mut input, steps).unwrap();
        drop(input);
        assert_eq!(work.used(), 0);
        let (registry, pending) = output.into_parts();
        assert!(previous.contains_key("public.removed"));
        assert!(!registry.contains_key("public.removed"));
        assert!(Arc::ptr_eq(
            &registry["pg_temp_7.kept"][0],
            &selected_temporary
        ));
        let mut actual = pending.groups().flatten();
        for (identity, parameters, name, body) in pointers {
            let routine = actual.next().unwrap();
            assert_eq!(routine.def.object_id, Some(identity));
            assert_eq!(routine.def.params.as_ptr(), parameters);
            assert_eq!(routine.def.name.as_ptr(), name);
            assert_eq!(
                match &routine.def.body {
                    FunctionBody::Source(text) => text.as_ptr() as usize,
                    FunctionBody::Statements(statements) => statements.as_ptr() as usize,
                },
                body
            );
            let RoutineBody::Bound(bound) = &routine.body else {
                panic!("ordinary placeholder form")
            };
            assert!(
                matches!(&***bound, crate::routines::CompiledFunctionBody::SQL(plans) if plans.is_empty())
            );
            assert!(routine.is_retained_by(&native));
            let stored = registry[&routine.def.name]
                .iter()
                .find(|candidate| candidate.def.object_id == Some(identity))
                .unwrap();
            assert!(Arc::ptr_eq(routine, stored));
        }
        assert!(actual.next().is_none());
        for (_, _, definitions) in fixture() {
            let mut expected = definitions.iter().collect::<Vec<_>>();
            expected.sort_by(|left, right| {
                routine_signature_types(left)
                    .cmp(&routine_signature_types(right))
                    .then_with(|| left.is_procedure.cmp(&right.is_procedure))
            });
            let name = RelationIdentity::from_legacy_name(&definitions[0].name)
                .unwrap()
                .qualified_name();
            assert!(registry[&name]
                .iter()
                .map(|routine| routine.def.object_id)
                .eq(expected.into_iter().map(|definition| definition.object_id)));
        }
        drop(actual);
        drop((registry, previous, selected_temporary));
        assert!(native.used() > 0);
        assert_eq!(pending.groups().flatten().count(), 6);
        drop(pending);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_routine_placeholders_cancel_or_abandon_at_every_pending_boundary_without_publication() {
    let fixture = vec![(
        "f".into(),
        true,
        vec![definition("f", 1, &["text"]), definition("f", 2, &["int"])],
    )];
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = placeholders(
        canonical(&fixture, &native, &work),
        RoutineRegistry::new(),
        &native,
        &work,
        &token,
        &token,
    );
    let (output, polls) = finish(&mut input, 1).unwrap();
    drop((input, output));
    assert!(polls > 20);
    assert_eq!((native.used(), work.used()), (0, 0));
    for point in 0..polls - 1 {
        for cancel in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let previous = RoutineRegistry::new();
            let mut input = placeholders(
                canonical(&fixture, &native, &work),
                previous.clone(),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..point {
                assert!(input
                    .advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            if let Some(invoking_owner) = cancel {
                if invoking_owner {
                    invoking.cancel();
                } else {
                    original.cancel();
                }
                assert!(matches!(
                    finish(&mut input, 1),
                    Err(RoutineRestoreInputError::Native(
                        uqa_core::ValueRetentionError::Cancelled(_)
                    ) | RoutineRestoreInputError::Work(ImageIoError::Cancelled(_)))
                ));
            }
            drop(input);
            assert!(previous.is_empty());
            assert_eq!(
                (native.used(), work.used()),
                (0, 0),
                "pending boundary {point}"
            );
        }
    }
}

#[test]
fn retained_routine_placeholders_preserve_long_signature_order_and_reject_native_admission_pressure(
) {
    let prefix = "name".repeat(4096);
    let first = format!("{prefix}a");
    let second = format!("{prefix}b");
    let groups = vec![(
        "f".into(),
        true,
        vec![
            definition("f", 1, &[&second]),
            definition("f", 2, &[&first]),
        ],
    )];
    let native = MemoryBudget::new(1 << 22);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = placeholders(
        canonical(&groups, &native, &work),
        RoutineRegistry::new(),
        &native,
        &work,
        &token,
        &token,
    );
    let (output, polls) = finish(&mut input, 1).unwrap();
    assert!(polls > first.len() / 4096);
    let (registry, pending) = output.into_parts();
    assert_eq!(registry["public.f"][0].def.object_id, Some([2; 16]));
    drop((input, registry, pending));
    for headroom in [0, 1, 128, 512, 1024] {
        let definitions = canonical(&groups, &native, &work);
        let hold = native.reserve(native.available() - headroom).unwrap();
        let mut input = placeholders(
            definitions,
            RoutineRegistry::new(),
            &native,
            &work,
            &token,
            &token,
        );
        assert!(matches!(
            finish(&mut input, 1),
            Err(RoutineRestoreInputError::Native(
                uqa_core::ValueRetentionError::Memory(_)
            ))
        ));
        drop(input);
        assert_eq!(native.used(), hold.bytes());
        drop(hold);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_routine_placeholders_preserve_temporary_replacement_and_empty_catalog_semantics() {
    for empty in [true, false] {
        let native = MemoryBudget::new(if empty { 0 } else { 1 << 20 });
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let previous = previous_registry();
        let groups = if empty {
            Vec::new()
        } else {
            vec![
                (
                    "pg_temp_7.kept".into(),
                    true,
                    vec![definition("pg_temp_7.kept", 9, &[])],
                ),
                ("f".into(), true, vec![definition("f", 10, &[])]),
            ]
        };
        let mut input = placeholders(
            canonical(&groups, &native, &work),
            previous.clone(),
            &native,
            &work,
            &token,
            &token,
        );
        let (output, _) = finish(&mut input, 1).unwrap();
        drop(input);
        let (registry, pending) = output.into_parts();
        assert_eq!(
            registry["pg_temp_7.kept"][0].def.object_id,
            Some([if empty { 7 } else { 9 }; 16])
        );
        assert_eq!(previous["pg_temp_7.kept"][0].def.object_id, Some([7; 16]));
        assert!(!registry.contains_key("public.removed"));
        assert_eq!(pending.groups().len(), if empty { 0 } else { 2 });
        let capture = MemoryBudget::new(1 << 20);
        let selected = registry
            .capture(&ProductionControl::new(&capture, &token, &token))
            .unwrap();
        let names = selected
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, if empty { vec![] } else { vec!["public.f"] });
        drop((selected, registry, pending));
        assert_eq!((native.used(), work.used(), capture.used()), (0, 0, 0));
    }
}
