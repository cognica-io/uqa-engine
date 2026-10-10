//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::expr::RowLookup;

struct StackCheckedRow {
    base: usize,
    visits: Cell<usize>,
    value: Value,
}

impl RowLookup for StackCheckedRow {
    fn column(&self, name: &str) -> Option<&Value> {
        assert_eq!(name, "v");
        let marker = 0u8;
        let used = self.base.abs_diff(std::ptr::from_ref(&marker) as usize);
        // Reject excessive growth before exhausting the ordinary test thread's stack.
        // Scalar dispatch must leave room within the existing native routine budget.
        assert!(
            used < 1_000_000,
            "scalar evaluation used {used} stack bytes"
        );
        self.visits.set(self.visits.get() + 1);
        Some(&self.value)
    }

    fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
        unreachable!("the synthetic expression uses an unqualified column")
    }
}

#[test]
fn nested_scalar_calls_fit_the_native_routine_stack_budget() {
    let mut expression = ScalarExpr::Literal(Value::Int(0));
    for _ in 0..32 {
        expression = ScalarExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(ScalarExpr::Column("v".into())),
            rhs: Box::new(ScalarExpr::Func {
                order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
                name: "abs".into(),
                binding: None,
                args: vec![expression],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            }),
        };
    }
    let marker = 0u8;
    let row = StackCheckedRow {
        base: std::ptr::from_ref(&marker) as usize,
        visits: Cell::new(0),
        value: Value::Int(1),
    };
    assert_eq!(
        eval_scalar(&expression, &ScalarEvalContext::from_row_lookup(&row, &[])).unwrap(),
        Value::Int(32)
    );
    assert_eq!(row.visits.get(), 32);
    row.visits.set(0);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = eval_generated_scalar_with_control(&expression, &row, &control).unwrap();
    assert_eq!(*value, Value::Int(32));
    assert_eq!(row.visits.get(), 32);
    drop(value);
    assert_eq!(budget.used(), 0);
}
