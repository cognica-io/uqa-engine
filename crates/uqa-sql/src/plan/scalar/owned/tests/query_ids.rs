//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{scalar::owned::query_ids::QueryIndexInput, ScalarInputWork},
    SQLError,
};
use std::result::Result;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

struct Mapper {
    calls: Arc<AtomicUsize>,
    fail_at: Option<usize>,
}

impl<W: ScalarInputWork> QueryIndexInput<W> for Mapper
where
    W::Error: Into<SQLError>,
{
    async fn index(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<usize, SQLError> {
        work.checkpoint().await.map_err(Into::into)?;
        control.check()?;
        let ordinal = self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_at == Some(ordinal) {
            return Err(SQLError::Internal(
                "selected query occurrence failed".into(),
            ));
        }
        Ok(index + 100 + ordinal)
    }
}

fn input(
    expression: OwnedScalarExpr,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    mut mapper: Mapper,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        expression
            .remap_query_occurrences_from(&control, &mut channel, &mut mapper)
            .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (OwnedScalarExpr, usize) {
    for polls in 1..250_000 {
        let mut next = None;
        let allocated = allocation_counter::measure(|| next = Some(advance(input)));
        assert!(i128::from(allocated.count_total) - i128::from(allocated.count_current) < 64);
        if let Some(value) = next.unwrap().unwrap() {
            return (value, polls);
        }
    }
    panic!("scalar-query remapping stopped progressing")
}

fn mapper() -> Mapper {
    Mapper {
        calls: Arc::new(AtomicUsize::new(0)),
        fail_at: None,
    }
}

fn query_index(expression: &mut ScalarExpr) -> Option<&mut usize> {
    match expression {
        ScalarExpr::ScalarSubquery(index)
        | ScalarExpr::Exists {
            subquery: index, ..
        }
        | ScalarExpr::InSubquery {
            subquery: index, ..
        } => Some(index),
        _ => None,
    }
}

fn nested_occurrences() -> ScalarExpr {
    ScalarExpr::Row(vec![
        ScalarExpr::InSubquery {
            expr: Box::new(ScalarExpr::ScalarSubquery(0)),
            subquery: 1,
            negated: true,
        },
        ScalarExpr::Exists {
            subquery: 2,
            negated: false,
        },
        ScalarExpr::ScalarSubquery(0),
    ])
}

#[test]
fn query_index_edits_preserve_distinct_visit_orders_and_original_field_owners() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let mut shapes = vec![nested_occurrences()];
    shapes.extend(fixtures::child_shapes().into_iter().map(|(mut shape, _)| {
        let mut next = 10;
        shape.visit_mut(&mut |node| {
            if matches!(node, ScalarExpr::Literal(_)) {
                *node = ScalarExpr::ScalarSubquery(next);
                next += 1;
            }
        });
        shape
    }));
    for (position, source) in shapes.into_iter().enumerate() {
        for before in [false, true] {
            let mut expected = source.clone();
            let mut reference_order = Vec::new();
            let mut remap = |node: &mut ScalarExpr| {
                if let Some(index) = query_index(node) {
                    let original = *index;
                    *index += 100 + reference_order.len();
                    reference_order.push(original);
                }
            };
            if before {
                expected.visit_mut(&mut remap);
            } else {
                crate::plan::rewrite_scalar_expression(&mut expected, &mut remap);
            }
            if position == 0 {
                assert_eq!(
                    reference_order,
                    if before {
                        vec![1, 0, 2, 0]
                    } else {
                        vec![0, 1, 2, 0]
                    }
                );
            }
            let value = source.copy_for_rewrite(&control).unwrap();
            let retained = value.reserved_bytes();
            let fields = value.memory.as_ref().unwrap().children.as_ptr();
            let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut map = |index| {
                let mut calls = calls.borrow_mut();
                let result = index + 100 + calls.len();
                calls.push(index);
                result
            };
            let actual = if before {
                value.remap_subquery_ids(&control, &mut map).unwrap()
            } else {
                value
                    .remap_query_occurrences(&control, &mut |index| Ok(map(index)))
                    .unwrap()
            };
            assert_eq!(*actual, expected);
            assert_eq!(*calls.borrow(), reference_order);
            assert_eq!(actual.reserved_bytes(), retained);
            assert_eq!(actual.memory.as_ref().unwrap().children.as_ptr(), fields);
            drop(actual);
            if !before {
                let value = source.copy_for_rewrite(&control).unwrap();
                let (actual, _) =
                    complete(&mut input(value, &native, &work, &token, &token, mapper()));
                assert_eq!(*actual, expected);
                assert_eq!(actual.reserved_bytes(), retained);
                drop(actual);
            }
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn query_index_edits_retire_every_interrupted_prefix_and_preserve_first_error() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = nested_occurrences();
    let control = ProductionControl::new(&native, &token, &token);
    let (_, polls) = complete(&mut input(
        source.copy_for_rewrite(&control).unwrap(),
        &native,
        &work,
        &token,
        &token,
        mapper(),
    ));
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for mode in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let mut input = input(
                source.copy_for_rewrite(&control).unwrap(),
                &native,
                &work,
                &original,
                &invoking,
                mapper(),
            );
            for _ in 0..prefix {
                assert!(advance(&mut input).unwrap().is_none());
            }
            if mode != 0 {
                if mode == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            let released = allocation_counter::measure(|| drop(input));
            assert_eq!(released.count_total, 0);
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    for fail_at in 0..4 {
        let map = Mapper {
            fail_at: Some(fail_at),
            ..mapper()
        };
        let calls = Arc::clone(&map.calls);
        let value = source.copy_for_rewrite(&control).unwrap();
        let error = input(value, &native, &work, &token, &token, map)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "internal error: selected query occurrence failed"
        );
        assert_eq!(calls.load(Ordering::Relaxed), fail_at + 1);
        assert_eq!((native.used(), work.used()), (37, 0));
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

fn deep(control: &ProductionControl<'_>, depth: usize) -> OwnedScalarExpr {
    let mut value = ScalarExpr::ScalarSubquery(7)
        .copy_for_rewrite(control)
        .unwrap();
    for _ in 0..depth {
        value = OwnedScalarExpr::cast(value, control.copy_text("int8").unwrap(), true, control)
            .unwrap();
    }
    value
}

#[test]
fn query_index_edits_bound_deep_traversal_and_unadmitted_scratch_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            for before in [false, true] {
                let value = deep(&control, 8192);
                let retained = native.used();
                let value = if before {
                    value
                        .remap_subquery_ids(&control, &mut |index| index + 1)
                        .unwrap()
                } else {
                    let (value, polls) =
                        complete(&mut input(value, &native, &work, &token, &token, mapper()));
                    assert!(polls > 8192);
                    value
                };
                assert_eq!(native.used(), retained);
                let mut leaf = &*value;
                for _ in 0..8192 {
                    let ScalarExpr::Cast { expr, .. } = leaf else {
                        panic!("retained cast")
                    };
                    leaf = expr;
                }
                assert_eq!(
                    *leaf,
                    ScalarExpr::ScalarSubquery(if before { 8 } else { 107 })
                );
                let released = allocation_counter::measure(|| drop(value));
                assert_eq!(released.count_total, 0);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
            let value = deep(&control, 32);
            let (_, polls) = complete(&mut input(value, &native, &work, &token, &token, mapper()));
            let mut denied = 0;
            for prefix in 0..polls {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut input = input(
                    deep(&control, 32),
                    &native,
                    &work,
                    &token,
                    &token,
                    Mapper {
                        calls: Arc::clone(&calls),
                        fail_at: None,
                    },
                );
                for _ in 0..prefix {
                    assert!(advance(&mut input).unwrap().is_none());
                }
                let occupied = native.reserve(native.available()).unwrap();
                loop {
                    match advance(&mut input) {
                        Ok(None) => {}
                        Ok(Some(value)) => {
                            drop(value);
                            break;
                        }
                        Err(error) => {
                            assert_eq!(error.sqlstate(), Some("53200"));
                            assert_eq!(calls.load(Ordering::Relaxed), 0);
                            denied += 1;
                            break;
                        }
                    }
                }
                let released = allocation_counter::measure(|| drop(input));
                assert_eq!(released.count_total, 0);
                assert_eq!(native.used(), occupied.bytes());
                drop(occupied);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
            assert!(denied > 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
