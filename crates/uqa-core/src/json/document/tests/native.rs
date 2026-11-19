//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{json::work::inline, Value as NativeValue};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

const VALUES: &[&str] = &[
    "null",
    "true",
    "false",
    "0",
    "-0",
    "-0.0",
    "1.00",
    "1e2",
    "1e-400",
    "9223372036854775807",
    "9223372036854775808",
    "18446744073709551615",
    "18446744073709551616",
    "-9223372036854775809",
    "1e400",
    "1e131072",
    r#""text\n한🙂\u0000""#,
    "[]",
    "[1,2]",
    "{}",
    r#"{"$uqa_type":"void"}"#,
    r#"{"$uqa_type":"float_bits","hex":"7ff0000000000000"}"#,
    r#"{"$uqa_type":"float_bits","hex":"fff8000000000001"}"#,
    r#"{"$uqa_type":"decimal","value":"1e400"}"#,
    r#"{"$uqa_type":"decimal","value":"-0.000"}"#,
    r#"{"$uqa_type":"decimal","value":"bad"}"#,
    r#"{"$uqa_type":"bytes","hex":"00ff80"}"#,
    r#"{"$uqa_type":"bytes","hex":"00fg"}"#,
    r#"{"$uqa_type":"bit_string","bit_len":9,"hex":"0080"}"#,
    r#"{"$uqa_type":"bit_string","bit_len":1,"hex":"81"}"#,
    r#"{"$uqa_type":"datum","type_oid":23,"offset":0,"hex":"00ff"}"#,
    r#"{"$uqa_type":"datum","type_oid":23,"offset":0,"hex":"zz"}"#,
    r#"{"$uqa_type":"enum","type_oid":4100,"key":"01"}"#,
    r#"{"$uqa_type":"enum","type_oid":false,"key":"01"}"#,
    r#"{"$uqa_type":"row","values":[1,2]}"#,
    r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":23,"type_modifier":-1,"collation_oid":null}]}"#,
    r#"{"$uqa_type":"row","values":[1],"field_types":[]}"#,
    r#"{"$uqa_type":"record","type_oid":4100,"fields":[["a",1],["a",2],["b",[1,2]]]}"#,
    r#"{"$uqa_type":"record","fields":[["a",1],[2,3]]}"#,
    r#"{"$uqa_type":"array","values":[[1,2],[3,4]],"lower_bounds":[-1,4]}"#,
    r#"{"$uqa_type":"array","values":[{"$uqa_type":"array","values":[1,2],"lower_bounds":[1]}],"lower_bounds":[1,1]}"#,
    r#"{"$uqa_type":"array","values":[[1],[2,3]],"lower_bounds":[1,1]}"#,
    r#"{"$uqa_type":"array","values":[],"lower_bounds":[]}"#,
    r#"{"$uqa_type":"int2vector","values":[1,2]}"#,
    r#"{"$uqa_type":"oidvector","values":[1,4294967295],"lower_bounds":[0]}"#,
    r#"{"$uqa_type":"date","days":42}"#,
    r#"{"$uqa_type":"time_tz_seconds","micros":1,"offset_seconds":3600}"#,
    r#"{"$uqa_type":"postgres_timestamp","postgres_micros":123456789}"#,
    r#"{"$uqa_type":"interval","months":1,"days":2,"micros":3}"#,
    r#"{"$uqa_type":"fixed_char","value":"a   "}"#,
    r#"{"$uqa_type":"json","value":"[1, 2]"}"#,
    r#"{"$uqa_type":"jsonb","value":"{\"a\":2}"}"#,
    r#"{"$uqa_type":"unknown","value":[1,2]}"#,
];

fn compare(text: &str) {
    let generic: serde_json::Value = serde_json::from_str(text).unwrap();
    let expected = serde_json::from_value::<NativeValue>(generic);
    let original = MemoryBudget::new(16 << 20);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&original, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = inline(
        source
            .root()
            .read_native_value_with_work(&control, &mut (), protocol()),
    );
    drop(source);
    assert_eq!(original.used(), 0);
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => {
            assert!(
                actual.has_same_representation(&expected),
                "{text}: {actual:?} / {expected:?}"
            );
            assert_eq!(
                actual.reserved_bytes(),
                actual
                    .retained_payload_bytes_with_control(&control)
                    .unwrap()
            );
            assert_eq!(actual.reserved_bytes(), memory.used());
        }
        (Err(JsonInputError::Diagnostic(actual)), Err(expected)) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.position().line, expected.line());
            assert_eq!(actual.position().column, expected.column());
            assert_eq!(actual.is_data(), expected.is_data());
        }
        (actual, expected) => {
            panic!("different projected literal: {text}: {actual:?} / {expected:?}")
        }
    }
    assert_eq!(memory.used(), 0, "{text}");
}

#[test]
fn projected_native_values_match_generic_numeric_visitors_and_every_tag_owner() {
    for text in VALUES {
        compare(text);
        compare(&format!("[{text},{{\"nested\":{text}}}]"));
    }
    for text in ["1.00", "1e400", "NaN", "bad"] {
        // An ordinary field before the protocol key preserves an actual map at physical generic input; select its nested value after construction.
        let encoded = format!(r#"{{"a":0,"{}":"{}"}}"#, protocol().number, text);
        compare(&encoded);
    }
}

#[test]
fn projected_native_scalars_need_no_destination_allocation_or_retained_source() {
    for text in ["null", "true", "0", "-1", "1.25", "18446744073709551615"] {
        let original = MemoryBudget::new(1 << 20);
        let destination = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let source = JsonDocument::read_with_control(
            text.as_bytes(),
            &ProductionControl::new(&original, &token, &token),
            protocol(),
        )
        .unwrap();
        let control = ProductionControl::new(&destination, &token, &token);
        let keys = protocol();
        let measured = allocation_counter::measure(|| {
            let value = inline(
                source
                    .root()
                    .read_native_value_with_work(&control, &mut (), keys),
            )
            .unwrap();
            assert_eq!(value.reserved_bytes(), 0);
        });
        assert_eq!(measured.count_total, 0, "{text}");
        drop(source);
        assert_eq!(original.used(), 0);
        assert_eq!(destination.used(), 0);
    }
}

struct Yielding {
    steps: usize,
    fail_at: Option<usize>,
}
impl JsonReadWork for Yielding {
    type Error = JsonReadError;
    async fn checkpoint(&mut self) -> std::result::Result<(), Self::Error> {
        self.steps += 1;
        if self.fail_at == Some(self.steps) {
            return Err(JsonReadError::InvalidJson);
        }
        let mut yielded = false;
        std::future::poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        Ok(())
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
}

#[test]
fn projected_native_values_resume_and_retire_failed_cancelled_and_abandoned_generations() {
    let text = r#"{"a":{"$uqa_type":"record","fields":[["name",{"$uqa_type":"decimal","value":"1e400"}]]},"b":{"$uqa_type":"array","values":[[1,2],[3,4]],"lower_bounds":[1,1]}}"#;
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&source_memory, &source_token, &source_token),
        protocol(),
    )
    .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut work = Yielding {
        steps: 0,
        fail_at: None,
    };
    let value = finish(source.root().read_native_value_with_work(
        &ProductionControl::new(&memory, &token, &token),
        &mut work,
        protocol(),
    ))
    .unwrap();
    let total = work.steps;
    let peak = memory.peak();
    assert!(total > 100);
    drop(value);
    assert_eq!(memory.used(), 0);
    for stop in 1..=total {
        for scope in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut work = Yielding {
                steps: 0,
                fail_at: (scope == 3).then_some(stop),
            };
            let mut future = Box::pin(source.root().read_native_value_with_work(
                &control,
                &mut work,
                protocol(),
            ));
            if scope == 3 {
                assert!(matches!(finish(future), Err(JsonInputError::Invalid)));
            } else {
                for _ in 0..stop {
                    assert!(future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending());
                }
                if scope != 0 {
                    if scope == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert!(matches!(finish(future), Err(JsonInputError::Cancelled(_))));
                } else {
                    drop(future);
                }
            }
            assert_eq!(memory.used(), 0, "{stop}/{scope}");
        }
    }
    let expected: NativeValue =
        serde_json::from_value(serde_json::from_str(text).unwrap()).unwrap();
    let mut rejected = 0;
    let mut completed = 0;
    for limit in [0, 1, peak / 2, peak - 1, peak] {
        let limited = MemoryBudget::new(limit);
        let control = ProductionControl::new(&limited, &token, &token);
        let mut work = Yielding {
            steps: 0,
            fail_at: None,
        };
        let result = finish(source.root().read_native_value_with_work(
            &control,
            &mut work,
            protocol(),
        ));
        match result {
            Ok(value) => {
                assert!(limit > 1);
                assert!(value.has_same_representation(&expected));
                assert_eq!(value.reserved_bytes(), limited.used());
                completed += 1;
            }
            Err(JsonInputError::Memory(_)) => rejected += 1,
            result => panic!("limit {limit}, peak {peak}: {result:?}"),
        }
        assert!(limited.peak() <= limit);
        assert_eq!(limited.used(), 0);
    }
    assert!(rejected >= 2);
    assert!(completed >= 1);
}

#[test]
fn projected_native_literals_keep_expanded_depth_and_destruction_on_the_original_small_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let inner = format!("{}0{}", "[".repeat(126), "]".repeat(126));
            let raw = format!(
                r#"{{"{}":{}}}"#,
                protocol().raw,
                serde_json::to_string(&inner).unwrap()
            );
            let text = format!("{}{}{}", "[".repeat(126), raw, "]".repeat(126));
            let memory = MemoryBudget::new(8 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let source =
                JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
            let value = inline(source.root().read_native_value_with_work(
                &control,
                &mut (),
                protocol(),
            ))
            .unwrap();
            drop(source);
            let mut child: &NativeValue = &value;
            for _ in 0..252 {
                let NativeValue::List(values) = child else {
                    panic!("expanded literal list");
                };
                child = &values[0];
            }
            assert_eq!(child, &NativeValue::Int(0));
            drop(value);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
