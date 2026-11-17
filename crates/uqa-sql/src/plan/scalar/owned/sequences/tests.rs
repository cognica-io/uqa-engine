//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{Arc, Mutex};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Visits = Arc<Mutex<Vec<(String, usize)>>>;
type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn scalar(sql: &str) -> OwnedScalarExpr {
    let crate::Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT")
    };
    OwnedScalarExpr::uncontrolled(
        crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar,
    )
}

fn input(
    value: OwnedScalarExpr,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    visits: Visits,
    fail: Option<&'static str>,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        value
            .bind_sequence_references_from_input(
                &mut |reference, control| {
                    if fail == Some(reference) {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: reference.into(),
                        });
                    }
                    let name = control.format(format_args!("catalog.{reference}"))?;
                    visits
                        .lock()
                        .unwrap()
                        .push((reference.into(), name.as_ptr() as usize));
                    Ok(name)
                },
                &control,
                &mut input,
            )
            .await
    })
    .unwrap()
}

fn step(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn finish(input: &mut Input) -> Result<(OwnedScalarExpr, usize), SQLError> {
    for polls in 1..100_000 {
        if let Some(value) = step(input)? {
            return Ok((value, polls));
        }
    }
    panic!("sequence binding stopped progressing")
}

#[test]
fn retained_sequence_binding_preserves_scalar_fields_order_and_catalog_buffers() {
    for sql in [
        "SELECT ROW(nextval('first'), currval('second'::regclass::regclass), setval('third', nextval('fourth')), app.nextval('ignored'), nextval('ignored'::text), nextval(7), nextval(NULL))",
        "SELECT sum(nextval('arg') ORDER BY currval('order')) FILTER (WHERE nextval('filter') > 0)",
        "SELECT sum(nextval('arg')) FILTER (WHERE nextval('filter') > 0) OVER (PARTITION BY nextval('partition') ORDER BY currval('order') ROWS BETWEEN nextval('start') PRECEDING AND currval('end') FOLLOWING)",
        "SELECT CASE WHEN nextval('condition') > 0 THEN pg_catalog.currval('true') ELSE PG_CATALOG.SETVAL('false', 1) END",
    ] {
        let source = scalar(sql);
        let mut expected = source.expression.clone();
        let mut order = Vec::new();
        crate::plan::rewrite_scalar_expression(&mut expected, &mut |node| {
            if let Some(reference) = crate::binding::view_dependencies::sequence_function_reference_mut(node) {
                order.push(reference.clone());
                *reference = format!("catalog.{reference}");
            }
        });
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let ready = source.copy_for_rewrite(&control).unwrap()
            .bind_sequence_references_with_control(&mut |name, control| Ok(control.format(format_args!("catalog.{name}"))?), &control).unwrap();
        let visits = Visits::default();
        let owned = source.copy_for_rewrite(&control).unwrap();
        let mut reader = input(owned, &native, &work, &token, &token, visits.clone(), None);
        let (resumed, _) = finish(&mut reader).unwrap();
        assert_eq!(*resumed, expected);
        assert_eq!(*ready, expected);
        let visits = visits.lock().unwrap();
        assert_eq!(visits.iter().map(|(name, _)| name).collect::<Vec<_>>(), order.iter().collect::<Vec<_>>());
        let mut pointers = Vec::new();
        resumed.visit(&mut |node| {
            if let ScalarExpr::Literal(Value::Str(text)) = node {
                if text.starts_with("catalog.") { pointers.push(text.as_ptr() as usize); }
            }
        });
        assert!(visits.iter().all(|(_, pointer)| pointers.contains(pointer)));
        assert_eq!(native.used(), ready.reserved_bytes() + resumed.reserved_bytes());
        drop((ready, resumed, reader));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_sequence_binding_retires_each_prefix_and_both_cancellation_scopes() {
    let source =
        scalar("SELECT ROW(nextval('first'::regclass), setval('second', currval('third')))");
    let measure = |stop: Option<(usize, usize)>| {
        let native = MemoryBudget::new(1 << 20);
        let sibling = native.reserve(37).unwrap();
        let work = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&native, &original, &invoking);
        let value = source.copy_for_rewrite(&control).unwrap();
        let mut reader = input(
            value,
            &native,
            &work,
            &original,
            &invoking,
            Visits::default(),
            None,
        );
        let mut polls = 0;
        loop {
            if let Some((limit, cancellation)) = stop {
                if limit == polls {
                    if cancellation == 0 {
                        let allocated = allocation_counter::measure(|| drop(reader));
                        assert_eq!(allocated.count_total, 0);
                    } else {
                        if cancellation == 1 {
                            original.cancel();
                        } else {
                            invoking.cancel();
                        }
                        assert_eq!(step(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                        let allocated = allocation_counter::measure(|| drop(reader));
                        assert_eq!(allocated.count_total, 0);
                    }
                    break;
                }
            }
            polls += 1;
            if let Some(value) = step(&mut reader).unwrap() {
                drop((value, reader));
                break;
            }
        }
        assert_eq!((native.used(), work.used()), (37, 0));
        drop(sibling);
        polls
    };
    let polls = measure(None);
    for stop in 0..polls {
        for cancellation in 0..3 {
            measure(Some((stop, cancellation)));
        }
    }
}

#[test]
fn retained_sequence_binding_preserves_first_failure_and_denied_admission() {
    let source = scalar("SELECT ROW(nextval('first'), nextval('missing'), nextval('last'))");
    let native = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let visits = Visits::default();
    let value = source.copy_for_rewrite(&control).unwrap();
    let mut reader = input(
        value,
        &native,
        &work,
        &token,
        &token,
        visits.clone(),
        Some("missing"),
    );
    assert_eq!(finish(&mut reader).unwrap_err().sqlstate(), Some("42P01"));
    assert_eq!(
        visits
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    drop(reader);
    assert_eq!((native.used(), work.used()), (37, 0));
    for available in [0, 1, 8, 32, 128] {
        let value = source.copy_for_rewrite(&control).unwrap();
        let held = native
            .reserve(native.limit() - native.used() - available)
            .unwrap();
        let used = held.bytes() + 37;
        let mut reader = input(
            value,
            &native,
            &work,
            &token,
            &token,
            Visits::default(),
            None,
        );
        let result = finish(&mut reader);
        if available <= 1 {
            assert!(
                result.is_err(),
                "canonical name cannot fit the remaining allowance"
            );
        }
        drop(result);
        drop(reader);
        assert_eq!((native.used(), work.used()), (used, 0));
        drop(held);
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_sequence_binding_bounds_wide_work_and_handles_deep_casts_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let literal = |name: &str| {
                OwnedScalarExpr::literal(
                    control.copy_value(&Value::Str(name.into())).unwrap(),
                    &control,
                )
                .unwrap()
            };
            for deep in [false, true] {
                let value = if deep {
                    let mut argument = literal("deep");
                    for _ in 0..8192 {
                        argument = OwnedScalarExpr::cast(
                            argument,
                            control.copy_text("regclass").unwrap(),
                            false,
                            &control,
                        )
                        .unwrap();
                    }
                    OwnedScalarExpr::call(
                        control.copy_text("nextval").unwrap(),
                        None,
                        [argument].into_iter(),
                        &control,
                    )
                    .unwrap()
                } else {
                    OwnedScalarExpr::row(
                        (0..2048).map(|_| {
                            OwnedScalarExpr::call(
                                control.copy_text("currval").unwrap(),
                                None,
                                [literal("wide")].into_iter(),
                                &control,
                            )
                            .unwrap()
                        }),
                        &control,
                    )
                    .unwrap()
                };
                let visits = Visits::default();
                let mut reader = input(value, &native, &work, &token, &token, visits.clone(), None);
                let mut polls = 0;
                loop {
                    let mut output = None;
                    let allocated =
                        allocation_counter::measure(|| output = step(&mut reader).unwrap());
                    polls += 1;
                    assert!(
                        allocated.count_total <= 16,
                        "allocations in one window: {}",
                        allocated.count_total
                    );
                    if let Some(value) = output {
                        let allocated = allocation_counter::measure(|| drop(value));
                        assert_eq!(allocated.count_total, 0);
                        break;
                    }
                    assert!(polls < 100_000);
                }
                assert!(polls > if deep { 8192 } else { 2048 });
                assert_eq!(visits.lock().unwrap().len(), if deep { 1 } else { 2048 });
                drop(reader);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
