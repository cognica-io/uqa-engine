//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{BinaryOp, Expr};

mod mixed;

#[test]
fn native_scalar_retirement_uses_constant_stack_without_allocating() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut value = Expr::Star;
            for _ in 0..10000 {
                value = Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(value),
                    rhs: Box::new(Expr::Literal(uqa_core::Value::Str("right".into()))),
                };
            }
            let allocations = allocation_counter::measure(|| drop(value));
            assert_eq!(allocations.count_total, 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_syntax_literals_retire_deep_values_and_original_composites_without_allocation() {
    fn value(depth: usize) -> uqa_core::Value {
        let mut value = uqa_core::Value::Str("leaf".into());
        for _ in 0..depth {
            value = uqa_core::Value::List(vec![value]);
        }
        value
    }
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for typed in [false, true] {
                let mut expr = if typed {
                    Expr::TypedLiteral {
                        value: value(8192),
                        ty: "public.record".into(),
                        collation_oid: None,
                        composite_source: Some(Box::new(
                            crate::expr::composites::CompositeConstantSource {
                                value: value(8192),
                                descriptors: Vec::new(),
                            },
                        )),
                    }
                } else {
                    Expr::Literal(value(8192))
                };
                for _ in 0..2048 {
                    expr = Expr::Not(Box::new(expr));
                }
                let released = allocation_counter::measure(|| drop(expr));
                assert_eq!(released.count_total, 0);
                assert!(released.count_current < -10_000);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
