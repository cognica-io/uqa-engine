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
