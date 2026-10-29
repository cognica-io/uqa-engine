//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Access ordering admits one permutation and moves child owners within their existing conjunction buffer.

use super::{operator_compatible, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionVec};
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

pub(in crate::optimizer) fn prioritize(
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    expression.rewrite(control, |expression| {
        if !matches!(&*expression, ScalarExpr::And(_)) {
            return Ok(expression);
        }
        let mut expression =
            expression.map_children(control, |_, child| prioritize(child, control))?;
        let ScalarExpr::And(items) = &*expression else {
            unreachable!("conjunction ordering")
        };
        let mut permutation = ProductionVec::new(*control);
        permutation.reserve(items.len())?;
        let mut access_count = 0;
        for item in items {
            let access = operator_compatible(item);
            access_count += usize::from(access);
            permutation.push_copy(usize::from(access))?;
        }
        let mut access = 0;
        let mut residual = access_count;
        for destination in permutation.as_mut_slice() {
            control.check()?;
            let next = if *destination == 1 {
                &mut access
            } else {
                &mut residual
            };
            *destination = *next;
            *next += 1;
        }
        for index in 0..permutation.len() {
            while permutation[index] != index {
                control.check()?;
                let destination = permutation[index];
                expression = expression.swap_children(index, destination, control)?;
                permutation.as_mut_slice().swap(index, destination);
            }
        }
        Ok(expression)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken, Value};
    use uqa_sql::ast::{BinaryOp, FunctionOrderSyntax};

    fn input() -> ScalarExpr {
        ScalarExpr::And(vec![
            ScalarExpr::Column("first_residual".repeat(1024).into()),
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(ScalarExpr::Column("first_access".repeat(1024).into())),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
            },
            ScalarExpr::Column("second_residual".into()),
            ScalarExpr::Func {
                name: "TEXT_MATCH".into(),
                binding: None,
                args: Vec::new(),
                distinct: false,
                order_by: Vec::new(),
                order_syntax: FunctionOrderSyntax::Ordinary,
                filter: None,
            },
        ])
    }

    #[test]
    fn stable_access_order_preserves_exact_child_and_container_ownership() {
        let source = input();
        let ScalarExpr::And(items) = &source else {
            unreachable!()
        };
        let expected = ScalarExpr::And([1, 3, 0, 2].map(|index| items[index].clone()).into());
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let ScalarExpr::And(items) = &*input else {
            unreachable!()
        };
        let pointer = items.as_ptr();
        let before = budget.used();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(prioritize(input, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(*output, expected);
        let ScalarExpr::And(items) = &*output else {
            unreachable!()
        };
        assert_eq!(items.as_ptr(), pointer);
        assert_eq!(allocations.bytes_current, 0);
        assert_eq!(budget.used(), before);
        assert_eq!(output.reserved_bytes(), before);
        drop(output);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn rejected_or_cancelled_access_order_releases_all_private_owners() {
        let source = input();
        for mode in 0..3 {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let input = source.copy_for_rewrite(&control).unwrap();
            let sibling = budget.reserve(budget.available()).unwrap();
            if mode == 1 {
                original.cancel();
            }
            if mode == 2 {
                invoking.cancel();
            }
            let error = prioritize(input, &control).unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some(if mode == 0 { "53200" } else { "57014" })
            );
            assert_eq!(budget.used(), sibling.bytes());
        }
    }
}
