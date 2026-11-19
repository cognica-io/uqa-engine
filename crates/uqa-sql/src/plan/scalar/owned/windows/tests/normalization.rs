//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod retirement;

type Rewrite = ImageDecoder<OwnedScalarExpr, SQLError>;
type Definitions = Arc<Pending<Vec<ScalarWindowSpec>>>;

fn rewrite(
    source: OwnedScalarExpr,
    definitions: Definitions,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    normalize: bool,
) -> Rewrite {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        if normalize {
            source
                .normalize_window_definitions_from(&definitions, &control, &mut input)
                .await
        } else {
            source
                .replace_window_spec_from(&definitions[0], &control, &mut input)
                .await
        }
    })
    .unwrap()
}

fn step(reader: &mut Rewrite) -> std::result::Result<Option<OwnedScalarExpr>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn definition(count: usize) -> Definitions {
    Arc::new(Pending::new(vec![ScalarWindowSpec {
        definition: Some(0),
        partition_by: (0..count)
            .map(|index| ScalarExpr::Literal(Value::Int(index as i64)))
            .collect(),
        order_by: Vec::new(),
        frame: None,
    }]))
}

fn selected_source() -> Arc<OwnedScalarExpr> {
    let mut source = fixture();
    let ScalarExpr::WindowCall { spec, .. } = &mut Arc::get_mut(&mut source).unwrap().expression
    else {
        unreachable!()
    };
    spec.definition = Some(0);
    source
}

#[test]
fn retained_window_rewrites_preserve_arguments_filters_and_constructor_owners() {
    let source = selected_source();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    for count in [0, 2, 7] {
        let definitions = definition(count);
        for normalize in [false, true] {
            let ready = source.copy_for_rewrite(&control).unwrap();
            let ready = if normalize {
                crate::catalog::work::inline(ready.normalize_window_definitions_from(
                    &definitions,
                    &control,
                    &mut (),
                ))
                .unwrap()
            } else {
                ready
                    .replace_window_spec(&definitions[0], &control)
                    .unwrap()
            };
            let owned = source.copy_for_rewrite(&control).unwrap();
            let ScalarExpr::WindowCall { args, filter, .. } = &*owned else {
                unreachable!()
            };
            let args_pointer = args.as_ptr();
            let filter_pointer = std::ptr::from_ref(filter.as_deref().unwrap());
            let mut reader = rewrite(
                owned,
                definitions.clone(),
                &native,
                &work,
                &token,
                &token,
                normalize,
            );
            let mut polls = 0;
            let resumed = loop {
                polls += 1;
                assert!(polls < 10_000);
                if let Some(value) = step(&mut reader).unwrap() {
                    break value;
                }
            };
            assert!(polls > count);
            let ScalarExpr::WindowCall {
                args, filter, spec, ..
            } = &*resumed
            else {
                unreachable!()
            };
            let ScalarExpr::WindowCall {
                args: original_args,
                filter: original_filter,
                ..
            } = &**source
            else {
                unreachable!()
            };
            assert_eq!(spec, &definitions[0]);
            assert_eq!(args, original_args);
            assert_eq!(filter, original_filter);
            assert_eq!(args.as_ptr(), args_pointer);
            assert_eq!(
                std::ptr::from_ref(filter.as_deref().unwrap()),
                filter_pointer
            );
            assert_eq!(*resumed, *ready);
            assert_eq!(resumed.reserved_bytes(), ready.reserved_bytes());
            assert_eq!(
                native.used(),
                resumed.reserved_bytes() + ready.reserved_bytes()
            );
            drop((reader, resumed, ready));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_window_rewrites_retire_every_interrupted_edit_and_traversal() {
    let source = selected_source();
    let definitions = definition(7);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    for normalize in [false, true] {
        let mut reader = rewrite(
            source.copy_for_rewrite(&control).unwrap(),
            definitions.clone(),
            &native,
            &work,
            &token,
            &token,
            normalize,
        );
        let mut polls = 0;
        while step(&mut reader).unwrap().is_none() {
            polls += 1;
            assert!(polls < 10_000);
        }
        drop(reader);
        let sibling = native.reserve(29).unwrap();
        for prefix in 0..=polls {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let mut reader = rewrite(
                    source.copy_for_rewrite(&control).unwrap(),
                    definitions.clone(),
                    &native,
                    &work,
                    &original,
                    &invoking,
                    normalize,
                );
                for _ in 0..prefix {
                    assert!(step(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(step(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                }
                let allocation = allocation_counter::measure(|| drop(reader));
                assert_eq!(allocation.count_total, 0);
                assert_eq!((native.used(), work.used()), (29, 0));
            }
        }
        drop(sibling);
        assert_eq!(native.used(), 0);
    }
}

fn call(slot: Option<usize>) -> ScalarExpr {
    ScalarExpr::WindowCall {
        name: "sum".into(),
        args: Vec::new(),
        spec: ScalarWindowSpec {
            definition: slot,
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: None,
        },
        filter: None,
        modifiers: crate::ast::WindowCallModifiers::default(),
    }
}

#[test]
fn window_normalization_preserves_argument_filter_and_specification_failure_order() {
    let mut definitions = definition(1);
    Arc::get_mut(&mut definitions).unwrap()[0].partition_by[0] =
        ScalarExpr::Literal(Value::Str("x".repeat(1 << 20)));
    for arguments_first in [false, true] {
        let mut source = call(None);
        let ScalarExpr::WindowCall {
            args, filter, spec, ..
        } = &mut source
        else {
            unreachable!()
        };
        if arguments_first {
            args.push(call(Some(0)));
        }
        *filter = Some(Box::new(call(Some(99))));
        spec.partition_by.push(call(Some(0)));
        let source = OwnedScalarExpr::uncontrolled(source);
        let [(ready, ready_peak), (resumed, resumed_peak)] = [false, true].map(|retained| {
            let native = MemoryBudget::new(64 << 10);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let owned = source.copy_for_rewrite(&control).unwrap();
            let error = if retained {
                rewrite(
                    owned,
                    definitions.clone(),
                    &native,
                    &work,
                    &token,
                    &token,
                    true,
                )
                .read(&mut std::io::empty())
                .unwrap_err()
            } else {
                crate::catalog::work::inline(owned.normalize_window_definitions_from(
                    &definitions,
                    &control,
                    &mut (),
                ))
                .unwrap_err()
            };
            assert_eq!((native.used(), work.used()), (0, 0));
            (error, native.peak())
        });
        if arguments_first {
            // Each work type has its own admitted future layout; both diagnostics must identify the same denied payload above that actual workspace.
            for (error, peak) in [(&ready, ready_peak), (&resumed, resumed_peak)] {
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!(
                    error.to_string(),
                    uqa_core::memory::MemoryError::Limit {
                        required: peak + (1 << 20),
                        limit: 64 << 10,
                    }
                    .to_string()
                );
            }
        } else {
            assert_eq!(ready.to_string(), resumed.to_string());
            assert!(ready
                .to_string()
                .contains("a call refers to a missing window definition"));
        }
    }
}

#[test]
fn window_normalization_retires_deep_pending_parents_without_recursion() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = call(Some(0));
            for _ in 0..8192 {
                source = ScalarExpr::Not(Box::new(source));
            }
            let source = OwnedScalarExpr::uncontrolled(source);
            let definitions = definition(2);
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let ready = crate::catalog::work::inline(
                source
                    .copy_for_rewrite(&control)
                    .unwrap()
                    .normalize_window_definitions_from(&definitions, &control, &mut ()),
            )
            .unwrap();
            let mut leaf = &*ready;
            for _ in 0..8192 {
                let ScalarExpr::Not(child) = leaf else {
                    panic!("original parent")
                };
                leaf = child;
            }
            let ScalarExpr::WindowCall { spec, .. } = leaf else {
                panic!("normalized leaf")
            };
            assert_eq!(spec, &definitions[0]);
            drop(ready);
            for prefix in [0, 1, 4, 1024, 8192, 12_000] {
                for cancellation in 0..3 {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let control = ProductionControl::new(&native, &original, &invoking);
                    let mut reader = rewrite(
                        source.copy_for_rewrite(&control).unwrap(),
                        definitions.clone(),
                        &native,
                        &work,
                        &original,
                        &invoking,
                        true,
                    );
                    for _ in 0..prefix {
                        assert!(step(&mut reader).unwrap().is_none());
                    }
                    if cancellation != 0 {
                        if cancellation == 1 {
                            original.cancel();
                        } else {
                            invoking.cancel();
                        }
                        assert_eq!(step(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                    }
                    let allocations = allocation_counter::measure(|| drop(reader));
                    assert_eq!(allocations.count_total, 0);
                    assert_eq!((native.used(), work.used()), (0, 0));
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn window_normalization_admits_ancestor_storage_before_detached_children_escape() {
    let mut source = call(Some(0));
    for _ in 0..32 {
        source = ScalarExpr::Not(Box::new(source));
    }
    let source = OwnedScalarExpr::uncontrolled(source);
    let definitions = definition(2);
    for retained in [false, true] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let owned = source.copy_for_rewrite(&control).unwrap();
        let blocker = native.reserve((1 << 20) - native.used()).unwrap();
        let error = if retained {
            rewrite(
                owned,
                definitions.clone(),
                &native,
                &work,
                &token,
                &token,
                true,
            )
            .read(&mut std::io::empty())
            .unwrap_err()
        } else {
            crate::catalog::work::inline(owned.normalize_window_definitions_from(
                &definitions,
                &control,
                &mut (),
            ))
            .unwrap_err()
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (blocker.bytes(), 0));
        drop(blocker);
        assert_eq!(native.used(), 0);
    }
}
