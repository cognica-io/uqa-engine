//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CreateFunction, Expr, Statement};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod authority;

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap()
}
fn inline<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("inline constructor unexpectedly suspended"),
    }
}

fn compare<T: Projected + DeserializeOwned + Serialize + std::fmt::Debug>(value: Value) {
    if let Some(difference) = difference::<T>(&value) {
        panic!("{value}: {difference}");
    }
}

fn difference<T: Projected + DeserializeOwned + Serialize + std::fmt::Debug>(
    value: &Value,
) -> Option<String> {
    let encoded = serde_json::to_vec(value).unwrap();
    let generic: Value = serde_json::from_slice(&encoded).unwrap();
    let expected = serde_json::from_value::<T>(generic);
    let original = MemoryBudget::new(64 << 20);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        &encoded,
        &ProductionControl::new(&original, &token, &token),
        protocol(),
    )
    .unwrap();
    let actual = inline(
        Input {
            value: source.root(),
            control: ProductionControl::new(&memory, &token, &token),
            protocol: protocol(),
            buffered: false,
        }
        .run::<T, _>(&mut ())
        .unwrap(),
    )
    .map(Produced::from);
    drop(source);
    assert_eq!(original.used(), 0);
    let difference = match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            let expected = serde_json::to_value(expected).unwrap();
            let actual = serde_json::to_value(&*actual).unwrap();
            (expected != actual).then(|| format!("expected {expected}, got {actual}"))
        }
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.position().line, expected.line());
            assert_eq!(actual.position().column, expected.column());
            assert_eq!(actual.is_data(), expected.is_data());
            (actual.to_string() != expected.to_string())
                .then(|| format!("expected {expected}; got {actual}"))
        }
        (expected, actual) => Some(format!("expected {expected:?}; got {actual:?}")),
    };
    assert_eq!(memory.used(), 0, "{value}");
    difference
}

#[test]
fn projected_statement_input_preserves_every_statement_body_and_query_shape() {
    for sql in super::super::declarations::tests::STATEMENTS
        .iter()
        .chain(super::super::tests::QUERIES)
    {
        for statement in crate::compile(sql).unwrap_or_else(|error| panic!("{sql}: {error}")) {
            let value = serde_json::to_value(&statement).unwrap();
            compare::<Statement>(value);
            if let Statement::CreateFunction(function) = statement {
                compare::<CreateFunction>(serde_json::to_value(function).unwrap());
            }
        }
    }
}

#[test]
fn projected_statement_input_preserves_omissions_types_and_exact_diagnostic_order() {
    let mut differences = std::collections::BTreeMap::new();
    for sql in super::super::declarations::tests::STATEMENTS
        .iter()
        .chain(super::super::tests::QUERIES)
    {
        for statement in crate::compile(sql).unwrap() {
            let value = serde_json::to_value(&statement).unwrap();
            super::super::tests::mutate(&value, &mut |value| {
                if let Some(difference) = difference::<Statement>(&value) {
                    differences.entry(difference).or_insert(value);
                }
            });
        }
    }
    assert!(differences.is_empty(), "{differences:#?}");
}

#[test]
fn projected_flattened_fields_preserve_buffering_before_selected_typed_readers() {
    let Statement::Select(select) = crate::compile("WITH q AS (SELECT 1) SELECT * FROM q")
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT fixture");
    };
    let cte = serde_json::to_value(&select.with[0]).unwrap();
    for (name, text) in [
        ("aaa", "18446744073709551616"),
        ("zzz", "-9223372036854775809"),
        ("zzz", "1e400"),
        ("insert", "null"),
    ] {
        let mut changed = cte.clone();
        changed
            .as_object_mut()
            .unwrap()
            .insert(name.into(), serde_json::from_str(text).unwrap());
        compare::<crate::ast::CTE>(changed);
    }
    for sql in [
        "CREATE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'",
        "WITH q AS (SELECT 1) SELECT * FROM q",
        "EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1",
    ] {
        let encoded = serde_json::to_value(crate::compile(sql).unwrap().remove(0)).unwrap();
        let fields = encoded
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_object()
            .unwrap();
        for (name, text) in [
            ("aaa", "18446744073709551616"),
            ("zzz", "-9223372036854775809"),
            ("zzz", "1e400"),
        ] {
            let mut copy = fields.clone();
            copy.insert(name.into(), serde_json::from_str(text).unwrap());
            let mut changed = encoded.clone();
            let Value::Object(variant) = &mut changed else {
                unreachable!()
            };
            *variant.values_mut().next().unwrap() = Value::Object(copy);
            compare::<Statement>(changed);
        }
    }
    for value in [
        json!({"Func":{"name":"f","args":[],"distinct":false,"order_by":[],"binding":{"name":"f","argument_types":[],"assignment":false,"composite_field":false}}}),
        json!({"Literal":u64::MAX}),
    ] {
        compare::<Expr>(value);
    }
}

#[test]
fn projected_expression_input_keeps_expanded_depth_on_the_original_small_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let leaf = r#"{"Literal":0}"#;
            let inner = format!("{}{}{}", r#"{"Not":"#.repeat(125), leaf, "}".repeat(125));
            let raw = format!(
                r#"{{"{}":{}}}"#,
                protocol().raw,
                serde_json::to_string(&inner).unwrap()
            );
            let text = format!("{}{}{}", r#"{"Not":"#.repeat(125), raw, "}".repeat(125));
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let source =
                JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
            let value: Produced<Expr> = inline(
                Input {
                    value: source.root(),
                    control,
                    protocol: protocol(),
                    buffered: false,
                }
                .run::<Expr, _>(&mut ())
                .unwrap(),
            )
            .unwrap()
            .into();
            drop(source);
            let mut child: &Expr = &value;
            for _ in 0..250 {
                let Expr::Not(value) = child else {
                    panic!("expanded Not expression");
                };
                child = value;
            }
            assert!(matches!(child, Expr::Literal(uqa_core::Value::Int(0))));
            drop(value);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Default)]
struct Yielding {
    steps: usize,
    fail_at: Option<usize>,
}

#[test]
fn projected_queries_and_column_types_share_the_original_small_stack() {
    let Statement::Select(query) = crate::compile("SELECT 0").unwrap().remove(0) else {
        panic!("query fixture");
    };
    let encoded = serde_json::to_string(&query).unwrap();
    let (prefix, suffix) = encoded.split_once(r#"{"Literal":0}"#).unwrap();
    let prefix = format!(r#"{{"ScalarSubquery":{prefix}"#);
    let suffix = format!("{suffix}}}");
    let inner = format!(
        "{}{}{}",
        prefix.repeat(24),
        r#"{"Literal":0}"#,
        suffix.repeat(24)
    );
    let raw = format!(
        r#"{{"{}":{}}}"#,
        protocol().raw,
        serde_json::to_string(&inner).unwrap()
    );
    let expressions = format!("{}{}{}", prefix.repeat(24), raw, suffix.repeat(24));
    let inner = format!(
        "{}{}{}",
        r#"{"Array":"#.repeat(125),
        r#""Integer""#,
        "}".repeat(125)
    );
    let raw = format!(
        r#"{{"{}":{}}}"#,
        protocol().raw,
        serde_json::to_string(&inner).unwrap()
    );
    let types = format!("{}{}{}", r#"{"Array":"#.repeat(125), raw, "}".repeat(125));
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let source =
                JsonDocument::read_with_control(expressions.as_bytes(), &control, protocol())
                    .unwrap();
            let value: Produced<Expr> = inline(
                Input {
                    value: source.root(),
                    control,
                    protocol: protocol(),
                    buffered: false,
                }
                .run::<Expr, _>(&mut ())
                .unwrap(),
            )
            .unwrap()
            .into();
            drop(source);
            let mut child: &Expr = &value;
            for _ in 0..48 {
                let Expr::ScalarSubquery(query) = child else {
                    panic!("scalar subquery");
                };
                child = &query.projections[0].expr;
            }
            assert!(matches!(child, Expr::Literal(uqa_core::Value::Int(0))));
            drop(value);
            assert_eq!(memory.used(), 0);
            let source =
                JsonDocument::read_with_control(types.as_bytes(), &control, protocol()).unwrap();
            let value: Produced<crate::ast::ColumnType> = inline(
                Input {
                    value: source.root(),
                    control,
                    protocol: protocol(),
                    buffered: false,
                }
                .run::<crate::ast::ColumnType, _>(&mut ())
                .unwrap(),
            )
            .unwrap()
            .into();
            drop(source);
            let mut child: &crate::ast::ColumnType = &value;
            for _ in 0..250 {
                let crate::ast::ColumnType::Array(element) = child else {
                    panic!("array type");
                };
                child = element;
            }
            assert_eq!(child, &crate::ast::ColumnType::Integer);
            drop(value);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

impl JsonReadWork for Yielding {
    type Error = JsonReadError;
    async fn checkpoint(&mut self) -> std::result::Result<(), Self::Error> {
        self.steps += 1;
        if self.fail_at == Some(self.steps) {
            return Err(JsonReadError::InvalidJson);
        }
        let mut suspended = false;
        std::future::poll_fn(|context| {
            if suspended {
                Poll::Ready(())
            } else {
                suspended = true;
                context.waker().wake_by_ref();
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
fn projected_statement_tasks_retire_every_suspended_failed_and_cancelled_owner() {
    let statement = crate::compile(
        "CREATE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'",
    )
    .unwrap()
    .remove(0);
    let encoded = serde_json::to_vec(&statement).unwrap();
    let expected: Value = serde_json::from_slice(&encoded).unwrap();
    let source_memory = MemoryBudget::new(8 << 20);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        &encoded,
        &ProductionControl::new(&source_memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut work = Yielding::default();
    let value = finish(Statement::from_projected_json_with_work(
        source.root(),
        &control,
        &mut work,
        protocol(),
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(&*value).unwrap(), expected);
    let total = work.steps;
    let peak = memory.peak();
    assert!(total > 0);
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
            let mut future = Box::pin(Statement::from_projected_json_with_work(
                source.root(),
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
    let mut rejected = 0;
    let mut completed = 0;
    for limit in [0, 1, peak / 2, peak - 1, peak] {
        let limited = MemoryBudget::new(limit);
        let control = ProductionControl::new(&limited, &token, &token);
        let mut work = Yielding::default();
        match finish(Statement::from_projected_json_with_work(
            source.root(),
            &control,
            &mut work,
            protocol(),
        )) {
            Ok(value) => {
                assert!(limit > 1);
                assert_eq!(serde_json::to_value(&*value).unwrap(), expected);
                assert_eq!(value.reserved_bytes(), limited.used());
                completed += 1;
            }
            Err(JsonInputError::Memory(_)) => rejected += 1,
            result => panic!("limit {limit}: {result:?}"),
        }
        assert!(limited.peak() <= limit);
        assert_eq!(limited.used(), 0);
    }
    assert!(rejected >= 2 && completed >= 1);
    drop(source);
    assert_eq!(source_memory.used(), 0);
}
