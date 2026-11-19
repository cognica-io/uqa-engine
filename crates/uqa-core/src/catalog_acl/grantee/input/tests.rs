//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    json::{JsonDocument, JsonScalarProtocol},
    memory::MemoryBudget,
    CancellationToken,
};

fn inline<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("inline work cannot suspend"),
    }
}

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol {
        number: "unused-number-protocol",
        raw: "unused-raw-protocol",
    }
}

fn compare(text: &str) {
    let generic: serde_json::Value = serde_json::from_str(text).unwrap();
    let expected = serde_json::from_value::<AclGrantee>(generic);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let document = JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
    let actual = inline(AclGrantee::from_json_value_with_work(
        document.root(),
        &control,
        &mut (),
    ));
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(*actual, expected, "{text}"),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.position().line, expected.line());
            assert_eq!(actual.position().column, expected.column());
        }
        (expected, actual) => panic!(
            "{text}: expected {expected:?}; actual success {}",
            actual.is_ok()
        ),
    }
    drop(document);
    assert_eq!(memory.used(), 0);
}

#[test]
fn projected_acl_grantee_preserves_tagged_legacy_and_untagged_diagnostics() {
    for text in [
        "null",
        "false",
        "1",
        "0.0",
        "[]",
        "{}",
        r#""PUBLIC""#,
        r#""public""#,
        r#""한🙂""#,
        r#""""#,
    ] {
        compare(text);
    }
    for kind in [
        r#""public""#,
        r#""role""#,
        r#""unknown""#,
        "0",
        "1",
        "2",
        "-0",
        "0.0",
        "null",
        "false",
        "[]",
        "{}",
    ] {
        compare(&format!(r#"{{"kind":{kind}}}"#));
        compare(&format!("[{kind}]"));
        for name in ["null", "false", "1", "[]", "{}", r#""PUBLIC""#, r#""한🙂""#] {
            compare(&format!(r#"{{"kind":{kind},"name":{name}}}"#));
            compare(&format!(r#"{{"kind":{kind},"name":{name},"extra":null}}"#));
            compare(&format!("[{kind},{name}]"));
            compare(&format!("[{kind},{name},null]"));
        }
    }
}

#[test]
fn projected_acl_grantee_retains_only_its_admitted_name_after_source_retirement() {
    for name in ["PUBLIC".to_owned(), "한🙂".repeat(8192)] {
        let text = serde_json::to_string(&AclGrantee::Role(name.clone())).unwrap();
        let source_memory = MemoryBudget::new(8 << 20);
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let document = JsonDocument::read_with_control(
            text.as_bytes(),
            &ProductionControl::new(&source_memory, &token, &token),
            protocol(),
        )
        .unwrap();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                inline(AclGrantee::from_json_value_with_work(
                    document.root(),
                    &control,
                    &mut (),
                ))
                .unwrap(),
            );
        });
        assert_eq!(
            usize::try_from(measured.bytes_current).unwrap(),
            memory.used()
        );
        assert!(usize::try_from(measured.bytes_max).unwrap() <= memory.peak());
        drop(document);
        assert_eq!(source_memory.used(), 0);
        assert_eq!(result.as_ref().unwrap().role_name(), Some(name.as_str()));
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}
