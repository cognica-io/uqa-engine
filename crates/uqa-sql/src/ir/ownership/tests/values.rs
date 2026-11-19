//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn value(depth: usize, width: usize) -> Value {
    let mut value = Value::Record(
        (0..width)
            .map(|i| (format!("field_{i}"), Value::Str("payload".into())))
            .collect::<Vec<_>>()
            .into(),
    );
    for _ in 0..depth {
        value = Value::List(vec![value]);
    }
    value
}

fn literal(typed: bool, depth: usize, width: usize) -> ScalarExpr {
    let value = value(depth, width);
    if typed {
        ScalarExpr::TypedLiteral {
            value,
            ty: "public.record".into(),
            collation_oid: None,
            composite_source: Some(Box::new(CompositeConstantSource {
                value: self::value(depth, width),
                descriptors: Vec::new(),
            })),
            bound_type: Some(ty(depth)),
            parameter_index: None,
        }
    } else {
        ScalarExpr::Literal(value)
    }
}

#[test]
fn native_scalar_literals_retire_deep_values_and_original_composites_on_small_stacks() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for typed in [false, true] {
                for stepped in [false, true] {
                    let source = literal(typed, 8192, 32);
                    let released = allocation_counter::measure(|| {
                        if stepped {
                            let mut retirement = Retirement::new(source);
                            for _ in 0..8192 {
                                assert!(!retirement.advance());
                            }
                            drop(retirement);
                        } else {
                            drop(crate::plan::OwnedScalarExpr::uncontrolled(source));
                        }
                    });
                    assert_eq!(released.count_total, 0);
                    assert!(released.count_current < -8192);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_scalar_literal_retirement_bounds_container_and_descriptor_release() {
    for typed in [false, true] {
        let mut retirement = Retirement::new(literal(typed, 128, 4096));
        let mut steps = 0;
        let mut complete = false;
        while !complete {
            let released = allocation_counter::measure(|| complete = retirement.advance());
            assert_eq!(released.count_total, 0);
            assert!(released.count_current >= -16, "{released:?}");
            steps += 1;
            assert!(steps < 65_536);
        }
        assert!(steps > 4096 * if typed { 2 } else { 1 });
    }
}

#[test]
fn native_scalar_literal_retirement_keeps_payload_leases_through_each_interruption() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    for typed in [false, true] {
        let source = literal(typed, 7, 3);
        let mut prefixes = 0;
        for prefix in 0..256 {
            let mut complete = false;
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut input = input(&source, &native, &work, &original, &invoking);
                let retained = native.used();
                assert!(retained > sibling.bytes());
                let exhausted = native.reserve(native.limit() - retained).unwrap();
                for _ in 0..prefix {
                    let mut result = None;
                    assert_eq!(
                        allocation_counter::measure(|| result = advance(&mut input).unwrap())
                            .count_total,
                        0
                    );
                    if result.is_some() {
                        complete = true;
                        break;
                    }
                    assert_eq!(native.used(), native.limit());
                }
                if !complete && cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
                }
                assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                drop(exhausted);
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
            prefixes += 1;
            if complete {
                break;
            }
        }
        assert!(prefixes > 7 && prefixes < 256);
        drop(crate::plan::OwnedScalarExpr::uncontrolled(source));
    }
}
