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

type Definitions = BTreeMap<String, Vec<serde_json::Value>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceCatalog {
    routine_catalog_format: u32,
    definitions: Definitions,
}

fn reference(
    text: Option<&str>,
    allows_migration: bool,
) -> StorageBackendResult<(u32, Definitions)> {
    let value: serde_json::Value = text
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    let (format, definitions) = if value.get("routine_catalog_format").is_some() {
        let catalog: ReferenceCatalog = serde_json::from_value(value)?;
        if !matches!(catalog.routine_catalog_format, 1..=3) {
            return Err(StorageBackendError::Other(format!(
                "unknown routine catalog format {}",
                catalog.routine_catalog_format
            )));
        }
        (catalog.routine_catalog_format, catalog.definitions)
    } else {
        (0, serde_json::from_value(value)?)
    };
    if format != 3 && !allows_migration {
        return Err(StorageBackendError::Other(
            "routine authority requires initial catalog migration".into(),
        ));
    }
    Ok((format, definitions))
}

fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("inline work cannot suspend"),
    }
}

fn compare(text: Option<&str>, migration: bool) {
    let expected = reference(text, migration);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = inline(RoutineCatalogDocument::read_with_work(
        text,
        migration,
        &control,
        &mut (),
    ));
    match (expected, actual) {
        (Ok((format, definitions)), Ok(actual)) => {
            assert_eq!(actual.format(), format);
            assert_eq!(actual.definitions().len(), definitions.len());
            for ((name, values), (expected_name, expected_values)) in
                actual.definitions().zip(definitions.iter())
            {
                assert_eq!(name, expected_name);
                assert_eq!(values.len(), expected_values.len());
                for (value, expected) in values.iter().zip(expected_values) {
                    assert_eq!(
                        value
                            .root()
                            .to_json_with_control(&control)
                            .unwrap()
                            .as_str(),
                        expected.to_string()
                    );
                }
            }
        }
        (Err(expected), Err(actual)) => assert_eq!(
            actual.to_string(),
            expected.to_string(),
            "{text:?}, migrate {migration}"
        ),
        (expected, actual) => panic!("{text:?}: {expected:?}, actual success {}", actual.is_ok()),
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn routine_catalog_document_preserves_wrapper_shape_versions_and_migration_order() {
    for migration in [false, true] {
        compare(None, migration);
        for text in [
            "{}",
            "[]",
            "null",
            "1",
            "\"text\"",
            r#"{"f":[]}"#,
            r#"{"f":null}"#,
            r#"{"f":[null,1,{},[]]}"#,
        ] {
            compare(Some(text), migration);
        }
        for format in [
            "0",
            "1",
            "2",
            "3",
            "4",
            "null",
            "true",
            "[]",
            "{}",
            "\"3\"",
            "3.0",
            "3e0",
            "4294967296",
        ] {
            for definitions in [
                "null",
                "[]",
                "{}",
                r#"{"f":null}"#,
                r#"{"f":[null,{}],"a":[1.00,0.000001]}"#,
            ] {
                let text =
                    format!(r#"{{"routine_catalog_format":{format},"definitions":{definitions}}}"#);
                compare(Some(&text), migration);
            }
            compare(
                Some(&format!(r#"{{"routine_catalog_format":{format}}}"#)),
                migration,
            );
        }
        for key in ["a", "definitio", "routine_catalog_formau", "z"] {
            for value in ["0", "null", r#"{"a":1}"#] {
                compare(
                    Some(&format!(
                        r#"{{"routine_catalog_format":3,"definitions":{{}},"{key}":{value}}}"#
                    )),
                    migration,
                );
            }
        }
    }
}

#[test]
fn routine_catalog_document_preserves_complete_input_and_repeated_generic_projection() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let protocol = crate::catalog::type_records::marker::protocol::keys(&control).unwrap();
    for key in [protocol.raw, protocol.number] {
        for payload in [r#""1""#, r#""[1,]""#, "null", "1", r#""{\"owner\":null}""#] {
            for version in [0, 3, 4] {
                let text = format!(
                    r#"{{"routine_catalog_format":{version},"definitions":{{"z":[{{"z":null,{key:?}:{payload}}}]}}}}"#
                );
                compare(Some(&text), false);
                compare(Some(&text), true);
            }
        }
    }
    let text = r#"{"routine_catalog_format":3,"definitions":{"f":[{"name":"한🙂"}]}}"#;
    for end in 0..=text.len() {
        if text.is_char_boundary(end) {
            compare(Some(&text[..end]), false);
        }
    }
    for text in [
        r#"{"routine_catalog_format":0,"routine_catalog_format":3,"definitions":{"z":null,"z":[]}}"#,
        r#"{"routine_catalog_format":3,"definitions":{"a":[{"x":"\ud800","x":null}]}}"#,
    ] {
        compare(Some(text), true);
    }
}

mod resources;
