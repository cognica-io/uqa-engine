//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBody, RoutineAclEntry},
    catalog::roles::RoleDefinition,
    routines::security::binding::BoundRoutineAuthority,
};
use uqa_core::memory::ProductionVec;

fn declaration() -> CreateFunction {
    let Statement::CreateFunction(function) =
        crate::compile("CREATE FUNCTION public.f() RETURNS int RETURN 7")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    *function
}

fn prepared(
    control: &ProductionControl<'_>,
    entries: Option<usize>,
) -> std::result::Result<Produced<BoundRoutineAuthority>, JsonInputError> {
    let owner = RoleDefinition::bootstrap().identity();
    let (execute_acl, memory) = if let Some(entries) = entries {
        let mut values = ProductionVec::new(*control);
        for index in 0..entries {
            values
                .push_produced(
                    control
                        .finish(
                            RoutineAclEntry {
                                role: (index != 0).then_some(owner),
                                grantor: owner,
                                grant_option: false,
                            },
                            control.empty_reservation(),
                        )
                        .map_err(JsonReadError::from)?,
                )
                .map_err(JsonReadError::from)?;
        }
        let (values, memory) = values.finish().map_err(JsonReadError::from)?.into_parts();
        (Some(values), memory)
    } else {
        (None, control.empty_reservation())
    };
    Ok(control
        .finish(BoundRoutineAuthority { owner, execute_acl }, memory)
        .map_err(JsonReadError::from)?)
}

fn compare_authority(value: &Value, entries: Option<usize>) {
    let memory = MemoryBudget::new(32 << 20);
    let source_memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let authority = prepared(&control, entries).unwrap();
    let address = authority.execute_acl.as_ref().map(|values| values.as_ptr());
    let encoded = serde_json::to_vec(value).unwrap();
    let mut reference: Value = serde_json::from_slice(&encoded).unwrap();
    if let Some(fields) = reference.as_object_mut() {
        fields.insert(
            "owner".into(),
            serde_json::to_value(authority.owner).unwrap(),
        );
        fields.insert(
            "execute_acl".into(),
            serde_json::to_value(&authority.execute_acl).unwrap(),
        );
    }
    let expected = serde_json::from_value::<CreateFunction>(reference);
    let source = JsonDocument::read_with_control(
        &encoded,
        &ProductionControl::new(&source_memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let actual = inline(CreateFunction::from_projected_json_with_authority(
        source.root(),
        authority,
        &control,
        &mut (),
        protocol(),
    ));
    drop(source);
    assert_eq!(source_memory.used(), 0);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(
                serde_json::to_value(&*actual).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert_eq!(
                actual.execute_acl.as_ref().map(|values| values.as_ptr()),
                address
            );
        }
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{value}");
            assert_eq!(actual.position().line, expected.line());
            assert_eq!(actual.position().column, expected.column());
        }
        (expected, actual) => panic!("{value}: expected {expected:?}, got {actual:?}"),
    }
    assert_eq!(memory.used(), 0, "{value}");
}

#[test]
fn projected_routine_authority_preserves_native_buffers_field_order_and_nested_declarations() {
    for entries in [None, Some(0), Some(2)] {
        let mut definition = declaration();
        definition.body =
            FunctionBody::Statements(vec![Statement::CreateFunction(Box::new(declaration()))]);
        let value = serde_json::to_value(definition).unwrap();
        compare_authority(&value, entries);
        super::super::super::tests::mutate(&value, &mut |value| compare_authority(&value, entries));
        for name in ["aaa", "execute_acl", "owner", "zzz"] {
            for literal in ["18446744073709551616", "-9223372036854775809", "1e400"] {
                let mut changed = value.clone();
                changed
                    .as_object_mut()
                    .unwrap()
                    .insert(name.into(), serde_json::from_str(literal).unwrap());
                compare_authority(&changed, entries);
            }
        }
    }
}

#[test]
fn projected_routine_authority_retires_every_suspended_transfer_and_failed_admission() {
    let source_memory = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        &serde_json::to_vec(&declaration()).unwrap(),
        &ProductionControl::new(&source_memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut work = Yielding::default();
    let value = finish(CreateFunction::from_projected_json_with_authority(
        source.root(),
        prepared(&control, Some(2)).unwrap(),
        &control,
        &mut work,
        protocol(),
    ))
    .unwrap();
    let expected = serde_json::to_value(&*value).unwrap();
    let total = work.steps;
    let peak = memory.peak();
    drop(value);
    assert_eq!(memory.used(), 0);
    for stop in 1..=total {
        for mode in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut work = Yielding {
                steps: 0,
                fail_at: (mode == 3).then_some(stop),
            };
            let mut future = Box::pin(CreateFunction::from_projected_json_with_authority(
                source.root(),
                prepared(&control, Some(2)).unwrap(),
                &control,
                &mut work,
                protocol(),
            ));
            if mode == 3 {
                assert!(matches!(finish(future), Err(JsonInputError::Invalid)));
            } else {
                for _ in 0..stop {
                    assert!(future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending());
                }
                if mode == 0 {
                    drop(future);
                } else {
                    if mode == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert!(matches!(finish(future), Err(JsonInputError::Cancelled(_))));
                }
            }
            assert_eq!(memory.used(), 0, "{stop}/{mode}");
        }
    }
    let mut completed = 0;
    for limit in [0, 1, peak / 2, peak - 1, peak] {
        let limited = MemoryBudget::new(limit);
        let control = ProductionControl::new(&limited, &token, &token);
        let result = prepared(&control, Some(2)).and_then(|authority| {
            finish(CreateFunction::from_projected_json_with_authority(
                source.root(),
                authority,
                &control,
                &mut Yielding::default(),
                protocol(),
            ))
        });
        match result {
            Ok(value) => {
                assert_eq!(serde_json::to_value(&*value).unwrap(), expected);
                assert_eq!(value.reserved_bytes(), limited.used());
                completed += 1;
            }
            Err(JsonInputError::Memory(_)) => (),
            other => panic!("limit {limit}: {other:?}"),
        }
        assert!(limited.peak() <= limit);
        assert_eq!(limited.used(), 0);
    }
    assert!(total > 0 && completed > 0);
    drop(source);
    assert_eq!(source_memory.used(), 0);
}
