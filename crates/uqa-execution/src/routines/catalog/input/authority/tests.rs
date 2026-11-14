//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_sql::{
    catalog::roles::RoleDefinition,
    routines::security::binding::{bind_legacy_authority, LegacyRoutineAclEntry},
};

type Decoder = ImageDecoder<RoutineCatalogAuthority, StorageBackendError>;
type Definitions = BTreeMap<String, Vec<serde_json::Value>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceCatalog {
    routine_catalog_format: u32,
    definitions: Definitions,
}

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter(["owner", "delegate"].into_iter().zip(1_u8..).map(
        |(name, id)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_000 + i64::from(id);
            role.object_id = [id; 16];
            (name.into(), role)
        },
    ))
    .unwrap()
}

// Preserve the ordinary serde prefix, before typed CreateFunction construction.
fn reference(
    text: &str,
    roles: &RoleDefinitions,
) -> StorageBackendResult<Vec<BoundRoutineAuthority>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let (format, definitions): (_, Definitions) = if value.get("routine_catalog_format").is_some() {
        let wrapper: ReferenceCatalog = serde_json::from_value(value)?;
        (wrapper.routine_catalog_format, wrapper.definitions)
    } else {
        (0, serde_json::from_value(value)?)
    };
    let mut output = Vec::new();
    for definition in definitions.values().flatten() {
        if format >= 2 {
            if definition.get("owner").is_none() || definition.get("execute_acl").is_none() {
                return Err(StorageBackendError::Other(
                    "current routine authority fields are missing".into(),
                ));
            }
        } else {
            let object = definition.as_object().ok_or_else(|| {
                StorageBackendError::Other("legacy routine definition is not an object".into())
            })?;
            let owner = object
                .get("owner")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    StorageBackendError::Other("legacy routine owner is missing".into())
                })?;
            let acl: Option<Vec<LegacyRoutineAclEntry>> = object
                .get("execute_acl")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?
                .flatten();
            output.push(
                bind_legacy_authority(owner, acl, roles, format == 0)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?,
            );
        }
    }
    Ok(output)
}

fn document(
    text: &str,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> RoutineCatalogDocument {
    let control = ProductionControl::new(memory, token, token);
    let mut work = ();
    let future = RoutineCatalogDocument::read_with_work(Some(text), true, &control, &mut work);
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value.unwrap(),
        Poll::Pending => panic!("inline input cannot suspend"),
    }
}

fn decoder(
    document: RoutineCatalogDocument,
    roles: RoleDefinitions,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> StorageBackendResult<Decoder> {
    let retained = memory.clone();
    let original = original.clone();
    Ok(ImageDecoder::new(
        memory,
        invoking,
        |mut reader| async move {
            document
                .bind_authority_from(&roles, &retained, &original, &mut reader.input())
                .await
        },
    )?)
}

fn advance(
    input: &mut Decoder,
    steps: usize,
) -> StorageBackendResult<Option<RoutineCatalogAuthority>> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn complete(
    input: &mut Decoder,
    steps: usize,
) -> StorageBackendResult<(RoutineCatalogAuthority, usize)> {
    for poll in 1..100_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, poll));
        }
    }
    panic!("routine authority input stopped progressing")
}

fn equal(actual: &RoutineCatalogAuthority, expected: &[BoundRoutineAuthority]) {
    assert_eq!(actual.legacy_authorities().len(), expected.len());
    for (actual, expected) in actual.legacy_authorities().zip(expected) {
        assert_eq!(actual.owner, expected.owner);
        assert_eq!(actual.execute_acl, expected.execute_acl);
    }
}

#[test]
fn routine_catalog_authority_preserves_complete_raw_validation_before_typed_bodies() {
    for format in 0..=3 {
        for first in [
            "null",
            "[]",
            "{}",
            r#"{"owner":null,"execute_acl":null}"#,
            r#"{"owner":"owner","execute_acl":null,"body":false}"#,
            r#"{"owner":"owner","execute_acl":[],"body":false}"#,
            r#"{"owner":"missing","execute_acl":[{"role":"PUBLIC","grant_option":false},{}]}"#,
            r#"{"owner":"owner","execute_acl":[{"role":"PUBLIC","grant_option":true}]}"#,
            r#"{"owner":"owner","execute_acl":[{"role":"delegate","grant_option":false}]}"#,
        ] {
            for second in ["{}", r#"{"owner":"missing","execute_acl":null}"#] {
                let definitions = format!(r#"{{"z":[{second}],"empty":[],"a":[{first}]}}"#);
                let text = if format == 0 {
                    definitions
                } else {
                    format!(r#"{{"routine_catalog_format":{format},"definitions":{definitions}}}"#)
                };
                let expected = reference(&text, &roles());
                let source = MemoryBudget::new(1 << 20);
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = decoder(
                    document(&text, &source, &token),
                    roles(),
                    &memory,
                    &token,
                    &token,
                )
                .unwrap();
                let actual = complete(&mut input, 1);
                match (expected, actual) {
                    (Ok(expected), Ok((actual, _))) => {
                        equal(&actual, &expected);
                        assert_eq!(actual.catalog().definitions().len(), 3);
                    }
                    (Err(expected), Err(actual)) => {
                        assert_eq!(actual.to_string(), expected.to_string(), "{text}");
                    }
                    (expected, actual) => panic!(
                        "{text}: expected {}, actual {}",
                        expected.is_ok(),
                        actual.is_ok()
                    ),
                }
                drop(input);
                assert_eq!(source.used(), 0);
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

mod resources;
