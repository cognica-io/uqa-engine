//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::catalog::{encoding, input::RoutineCatalogDocument};
use serde_json::{json, Value};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_sql::catalog::roles::{RoleDefinition, RoleDefinitions};

type Decoder = ImageDecoder<RoutineCatalogDefinitions, StorageBackendError>;

fn roles() -> RoleDefinitions {
    let mut owner = RoleDefinition::bootstrap();
    owner.name = "owner".into();
    owner.oid = 20_001;
    owner.object_id = [7; 16];
    RoleDefinitions::try_from_iter([("owner".into(), owner)]).unwrap()
}

fn definition(format: u32) -> Value {
    let uqa_sql::Statement::CreateFunction(mut function) =
        uqa_sql::compile("CREATE FUNCTION public.f() RETURNS int RETURN 7")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    function.object_id = Some([3; 16]);
    function.owner = Some(roles()["owner"].identity());
    let mut value = serde_json::to_value(function).unwrap();
    if format < 2 {
        value["owner"] = "owner".into();
    }
    value
}

fn catalog(format: u32, first: Value, second: Value) -> String {
    let definitions = json!({"a":[first],"empty":[],"z":[second]});
    if format == 0 {
        definitions
    } else {
        json!({"routine_catalog_format":format,"definitions":definitions})
    }
    .to_string()
}

fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("inline generic input cannot suspend"),
    }
}

fn decoder(
    text: &str,
    roles: RoleDefinitions,
    source: &MemoryBudget,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> StorageBackendResult<Decoder> {
    let document = inline(RoutineCatalogDocument::read_with_work(
        Some(text),
        true,
        &ProductionControl::new(source, original, original),
        &mut (),
    ))?;
    let retained = memory.clone();
    let original = original.clone();
    Ok(ImageDecoder::new(
        memory,
        invoking,
        |mut reader| async move {
            let mut input = reader.input();
            let prepared = document
                .bind_authority_from(&roles, &retained, &original, &mut input)
                .await?;
            drop(roles);
            prepared.decode_from(&retained, &original, &mut input).await
        },
    )?)
}

fn advance(
    input: &mut Decoder,
    steps: usize,
) -> StorageBackendResult<Option<RoutineCatalogDefinitions>> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn complete(
    input: &mut Decoder,
    steps: usize,
) -> StorageBackendResult<(RoutineCatalogDefinitions, usize)> {
    for poll in 1..100_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, poll));
        }
    }
    panic!("typed routine input stopped progressing")
}

fn observe(output: &RoutineCatalogDefinitions) -> Value {
    serde_json::to_value(
        output
            .definitions()
            .map(|(name, values)| {
                (
                    name,
                    values.iter().map(|value| &**value).collect::<Vec<_>>(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap()
}

fn compare(text: &str) {
    let expected = encoding::decode(Some(text), &roles(), true);
    let source = MemoryBudget::new(32 << 20);
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let actual = decoder(text, roles(), &source, &memory, &token, &token)
        .and_then(|mut input| complete(&mut input, 1).map(|(value, _)| value));
    match (expected, actual) {
        (Ok((expected, migration)), Ok(actual)) => {
            assert_eq!(observe(&actual), serde_json::to_value(expected).unwrap());
            assert_eq!(actual.requires_migration(), migration);
        }
        (Err(expected), Err(actual)) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
        }
        (expected, actual) => panic!(
            "{text}: expected success {}, got success {}",
            expected.is_ok(),
            actual.is_ok()
        ),
    }
    assert_eq!(source.used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn typed_routine_catalog_preserves_current_and_legacy_definitions_and_empty_groups() {
    for format in 0..=3 {
        let original = definition(format);
        for acl in [
            Value::Null,
            json!([]),
            if format < 2 {
                json!([{"role":"PUBLIC","grant_option":false}])
            } else {
                json!([{"role":null,"grantor":roles()["owner"].identity(),"grant_option":false}])
            },
        ] {
            let mut changed = original.clone();
            changed["execute_acl"] = acl;
            compare(&catalog(format, changed, original.clone()));
        }
        let mut absent = original.clone();
        absent.as_object_mut().unwrap().remove("execute_acl");
        compare(&catalog(format, absent, original.clone()));
        for name in [
            "owner",
            "execute_acl",
            "name",
            "body",
            "returns",
            "security",
            "object_id",
            "zzz",
        ] {
            for replacement in [
                Value::Null,
                json!(false),
                json!([]),
                json!({}),
                json!("invalid"),
            ] {
                let mut changed = original.clone();
                changed
                    .as_object_mut()
                    .unwrap()
                    .insert(name.into(), replacement);
                compare(&catalog(format, changed, original.clone()));
            }
        }
    }
}

#[test]
fn typed_routine_catalog_finishes_all_raw_authority_and_bodies_before_semantic_validation() {
    for format in 0..=3 {
        let mut malformed_body = definition(format);
        malformed_body["body"] = false.into();
        let mut missing_owner = definition(format);
        missing_owner["owner"] = if format < 2 {
            "missing".into()
        } else {
            Value::Null
        };
        compare(&catalog(
            format,
            malformed_body.clone(),
            missing_owner.clone(),
        ));
        compare(&catalog(format, missing_owner, malformed_body.clone()));
        let mut missing_identity = definition(format);
        missing_identity["object_id"] = Value::Null;
        compare(&catalog(format, missing_identity, malformed_body));
    }
}

#[test]
fn typed_routine_catalog_retains_selected_roles_and_retires_terminal_work() {
    let text = catalog(1, definition(1), definition(1));
    let expected =
        serde_json::to_value(encoding::decode(Some(&text), &roles(), true).unwrap().0).unwrap();
    let token = CancellationToken::new();
    let mut total = 0;
    for steps in [1, 7, 64] {
        let source = MemoryBudget::new(8 << 20);
        let memory = MemoryBudget::new(8 << 20);
        let mut current = roles();
        let mut input = decoder(&text, current.clone(), &source, &memory, &token, &token).unwrap();
        assert!(advance(&mut input, 1).unwrap().is_none());
        current.remove("owner").unwrap();
        drop(current);
        let (output, polls) = complete(&mut input, steps).unwrap();
        assert_eq!(observe(&output), expected);
        if steps == 1 {
            total = polls + 1;
        }
        assert!(advance(&mut input, 1).is_err());
        drop(input);
        assert!(source.used() > 0 && memory.used() > 0);
        assert_eq!(output.definitions().nth(1).unwrap().0, "empty");
        drop(output);
        assert_eq!(source.used(), 0);
        assert_eq!(memory.used(), 0);
    }
    for stop in [0, 1, total / 2, total - 1] {
        for mode in 0..4 {
            let source = MemoryBudget::new(8 << 20);
            let memory = MemoryBudget::new(8 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input =
                decoder(&text, roles(), &source, &memory, &original, &invoking).unwrap();
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            match mode {
                0 => (),
                1 | 2 => {
                    if mode == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert!(matches!(
                        advance(&mut input, 1),
                        Err(StorageBackendError::Cancelled(_))
                    ));
                    assert!(advance(&mut input, 1).is_err());
                }
                3 => assert!(advance(&mut input, 0).is_err()),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(source.used(), 0);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn typed_routine_catalog_retains_missing_identity_diagnostics_until_redaction() {
    let name = "routine한🙂".repeat(8192);
    let mut first = definition(3);
    first["name"] = name.clone().into();
    first["object_id"] = Value::Null;
    let text = catalog(3, first, definition(3));
    let expected = encoding::decode(Some(&text), &roles(), true)
        .err()
        .unwrap()
        .to_string();
    let source = MemoryBudget::new(32 << 20);
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(&text, roles(), &source, &memory, &token, &token).unwrap();
    let error = complete(&mut input, 1).err().unwrap();
    drop(input);
    assert_eq!(source.used(), 0);
    assert_eq!(error.to_string(), expected);
    assert!(memory.used() >= name.len());
    let redacted = uqa_storage::backup::BackupError::from(error);
    assert!(!redacted.to_string().contains(&name));
    assert_eq!(memory.used(), 0);
}

#[test]
fn typed_routine_catalog_admission_failures_retire_partial_groups() {
    for format in 0..=3 {
        let text = catalog(format, definition(format), definition(format));
        let expected =
            serde_json::to_value(encoding::decode(Some(&text), &roles(), true).unwrap().0).unwrap();
        let source = MemoryBudget::new(8 << 20);
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let output = decoder(&text, roles(), &source, &memory, &token, &token)
            .and_then(|mut input| complete(&mut input, 1))
            .unwrap()
            .0;
        let peak = memory.peak();
        drop(output);
        assert_eq!(source.used(), 0);
        assert_eq!(memory.used(), 0);
        let mut completed = 0;
        let mut rejected = 0;
        for limit in [0, 1, 128, 4096, peak / 4, peak / 2, peak - 1, peak] {
            let limited = MemoryBudget::new(limit);
            let result = decoder(&text, roles(), &source, &limited, &token, &token)
                .and_then(|mut input| complete(&mut input, 1));
            match result {
                Ok((output, _)) => {
                    assert_eq!(observe(&output), expected);
                    completed += 1;
                }
                Err(StorageBackendError::Memory(_)) => rejected += 1,
                Err(error) => panic!("format {format}, limit {limit}: {error:?}"),
            }
            assert!(limited.peak() <= limit);
            assert_eq!(source.used(), 0);
            assert_eq!(limited.used(), 0);
        }
        assert!(rejected >= 2 && completed >= 1);
    }
}
