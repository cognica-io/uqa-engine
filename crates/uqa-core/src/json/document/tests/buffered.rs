//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::json::{work::inline, JsonDiagnostic};
use serde::Deserialize;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

#[derive(Deserialize)]
struct Field<T> {
    value: T,
}
#[derive(Deserialize)]
struct Flatten<T> {
    #[serde(flatten)]
    fields: Field<T>,
}

fn same<T: std::fmt::Debug + PartialEq>(
    expected: Result<T, serde_json::Error>,
    actual: Result<T, JsonInputError>,
    text: &str,
) {
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{text}"),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.is_syntax(), expected.is_syntax());
            assert_eq!(actual.position().line, expected.line());
            assert_eq!(actual.position().column, expected.column());
        }
        (expected, actual) => {
            panic!("buffered projection differs for {text}: {expected:?}, {actual:?}")
        }
    }
}

#[test]
fn generic_document_buffering_preserves_flattened_numeric_widths_and_diagnostics() {
    for scalar in [
        "null",
        "true",
        "[]",
        "{}",
        "\"한🙂\"",
        "-0",
        "0",
        "1.0",
        "1.00",
        "1e0",
        "-1",
        "255",
        "256",
        "32767",
        "32768",
        "-32768",
        "-32769",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "340282366920938463463374607431768211455",
        "340282366920938463463374607431768211456",
        "-170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105729",
        "1e40",
        "1e400",
        "1e-99999",
        "0.000001",
        "-0.0",
    ] {
        let text = format!("{{\"value\":{scalar}}}");
        let generic: serde_json::Value = serde_json::from_str(&text).unwrap();
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let source =
            JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
        macro_rules! compare {
            ($ty:ty, $reader:ident) => {{
                let expected =
                    serde_json::from_value::<Flatten<$ty>>(generic.clone()).map(|v| v.fields.value);
                let actual = inline(async {
                    let buffered = source
                        .root()
                        .buffer_with_work(&control, &mut (), protocol())
                        .await?;
                    buffered
                        .root()
                        .get("value")
                        .unwrap()
                        .$reader(&control, &mut ())
                        .await
                });
                same(expected, actual, &format!("{}: {text}", stringify!($ty)));
            }};
        }
        compare!(u8, read_buffered_u8_with_work);
        compare!(u32, read_buffered_u32_with_work);
        compare!(std::num::NonZeroU32, read_buffered_nonzero_u32_with_work);
        compare!(u64, read_buffered_u64_with_work);
        compare!(usize, read_buffered_usize_with_work);
        compare!(i16, read_buffered_i16_with_work);
        compare!(i32, read_buffered_i32_with_work);
        compare!(i64, read_buffered_i64_with_work);
        compare!(f32, read_buffered_f32_with_work);
        let expected = serde_json::from_value::<Flatten<bool>>(generic).map(|v| v.fields.value);
        let actual = inline(async {
            let buffered = source
                .root()
                .buffer_with_work(&control, &mut (), protocol())
                .await?;
            let value = buffered.root().get("value").unwrap();
            match value.as_bool() {
                Some(value) => Ok(value),
                None => Err(JsonDiagnostic::buffered_type_with_work(
                    value,
                    "a boolean",
                    &control,
                    &mut (),
                )
                .await?
                .into()),
            }
        });
        same(expected, actual, &format!("bool: {text}"));
        drop(source);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn generic_document_buffering_observes_unknown_fields_before_flattened_typed_errors() {
    for text in [
        r#"{"value":false,"z":18446744073709551616}"#,
        r#"{"a":[{"b":-9223372036854775809}],"value":false}"#,
        r#"{"value":false,"z":340282366920938463463374607431768211456}"#,
    ] {
        let generic: serde_json::Value = serde_json::from_str(text).unwrap();
        let expected = serde_json::from_value::<Flatten<u32>>(generic).map(|v| v.fields.value);
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let source =
            JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
        let actual = inline(async {
            let buffered = source
                .root()
                .buffer_with_work(&control, &mut (), protocol())
                .await?;
            buffered
                .root()
                .get("value")
                .unwrap()
                .read_buffered_u32_with_work(&control, &mut ())
                .await
        });
        same(expected, actual, text);
        drop(source);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn generic_document_buffering_adopts_native_allocations_and_preserves_scalar_maps() {
    let long = format!("1{}", "0".repeat(8192));
    let text = format!(
        r#"{{"text":"{}","values":[1.0,0.000001,1.00,1e400,{}]}}"#,
        "한🙂".repeat(8192),
        long
    );
    let original = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&original, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            inline(
                source
                    .root()
                    .buffer_with_work(&control, &mut (), protocol()),
            )
            .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(measured.bytes_max).unwrap() <= memory.peak());
    drop(source);
    assert_eq!(original.used(), 0);
    let output = output.unwrap();
    assert_eq!(
        output.root().get("text").unwrap().as_str().unwrap().len(),
        "한🙂".len() * 8192
    );
    let values: Vec<_> = output
        .root()
        .get("values")
        .unwrap()
        .elements()
        .unwrap()
        .collect();
    assert_eq!(values[0].number_text(), Some("1.0"));
    assert_eq!(values[1].number_text(), Some("1e-6"));
    assert_eq!(
        values[2].get(protocol().number).unwrap().as_str(),
        Some("1.00")
    );
    assert_eq!(
        values[3].get(protocol().number).unwrap().as_str(),
        Some(
            serde_json::from_str::<serde_json::Number>("1e400")
                .unwrap()
                .to_string()
                .as_str()
        )
    );
    assert_eq!(
        values[4].get(protocol().number).unwrap().as_str(),
        Some(long.as_str())
    );
    drop(output);
    assert_eq!(memory.used(), 0);
}

struct Yield;
impl JsonReadWork for Yield {
    type Error = JsonReadError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        let mut suspended = false;
        std::future::poll_fn(|_| {
            if suspended {
                Poll::Ready(Ok(()))
            } else {
                suspended = true;
                Poll::Pending
            }
        })
        .await
    }
}

#[test]
fn generic_document_buffering_resumes_and_retires_failed_or_cancelled_generations() {
    let text = format!(
        r#"{{"a":"{}","n":1.00,"z":[true,1.0]}}"#,
        "한🙂".repeat(4096)
    );
    let original = MemoryBudget::new(8 << 20);
    let source_token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&original, &source_token, &source_token),
        protocol(),
    )
    .unwrap();
    for cutoff in [1, 3, 8, 20, 40, 100] {
        for owner in [0, 1, 2] {
            let memory = MemoryBudget::new(8 << 20);
            let first = CancellationToken::new();
            let second = CancellationToken::new();
            let control = ProductionControl::new(&memory, &first, &second);
            let mut work = Yield;
            let mut future = Box::pin(source.root().buffer_with_work(
                &control,
                &mut work,
                protocol(),
            ));
            let mut cancelled = false;
            let mut completed = false;
            for count in 0..2000 {
                if count == cutoff {
                    match owner {
                        0 => first.cancel(),
                        1 => second.cancel(),
                        _ => (),
                    }
                    cancelled = owner != 2;
                }
                match future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                {
                    Poll::Pending => (),
                    Poll::Ready(result) => {
                        if cancelled {
                            assert!(matches!(result, Err(JsonInputError::Cancelled(_))));
                        } else {
                            assert!(result.is_ok());
                        }
                        completed = true;
                        break;
                    }
                }
            }
            assert!(completed);
            drop(future);
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [0, 64, 512, 4096, 16384] {
        let memory = MemoryBudget::new(limit);
        let control = ProductionControl::new(&memory, &source_token, &source_token);
        assert!(matches!(
            inline(
                source
                    .root()
                    .buffer_with_work(&control, &mut (), protocol())
            ),
            Err(JsonInputError::Memory(_))
        ));
        assert_eq!(memory.used(), 0);
    }
    drop(source);
    assert_eq!(original.used(), 0);
}

#[test]
fn generic_document_buffering_preserves_expanded_depth_on_the_original_small_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let inner = format!("{}null{}", "[".repeat(126), "]".repeat(126));
            let text = format!(
                "{}{{{:?}:{inner:?}}}{}",
                "[".repeat(126),
                protocol().raw,
                "]".repeat(126)
            );
            let memory = MemoryBudget::new(8 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let source =
                JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
            let buffered = inline(
                source
                    .root()
                    .buffer_with_work(&control, &mut (), protocol()),
            )
            .unwrap();
            drop(source);
            let mut value = buffered.root();
            let mut depth = 0;
            while let Some(mut values) = value.elements() {
                value = values.next().unwrap();
                depth += 1;
            }
            assert_eq!(depth, 252);
            assert_eq!(value.kind(), JsonValueKind::Null);
            drop(buffered);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
