//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function state belongs to immutable expressions prepared for one execution.

use uqa_sql::{
    ast::{BinaryOp, FunctionDispatch},
    expr::enums::EnumComparisonState,
    ScalarExpr,
};

/// Read-only lookup into the function state owned by a prepared expression set.
/// The state has one fixed-size entry per ordering call, never per input row.
#[derive(Debug, Default)]
pub struct FunctionCallStates {
    enums: Vec<(usize, EnumComparisonState)>,
}

impl FunctionCallStates {
    fn register(&mut self, expression: &ScalarExpr) {
        expression.visit(&mut |expression| {
            let address = match expression {
                ScalarExpr::Func {
                    binding: Some(binding),
                    args,
                    ..
                } if binding.builtin
                    && args.len() == 2
                    && matches!(binding.dispatch, Some(FunctionDispatch::Enum { operation, .. }) if operation.uses_comparison_state()) =>
                {
                    args.as_ptr() as usize
                }
                ScalarExpr::Binary {
                    op: BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual,
                    lhs,
                    ..
                } => std::ptr::from_ref(lhs.as_ref()) as usize,
                _ => return,
            };
            self.enums.push((address, EnumComparisonState::default()));
        });
    }

    pub(crate) fn enum_comparison(&self, arguments: &[ScalarExpr]) -> Option<&EnumComparisonState> {
        self.at(arguments.as_ptr() as usize)
    }

    pub(crate) fn enum_binary_comparison(&self, left: &ScalarExpr) -> Option<&EnumComparisonState> {
        self.at(std::ptr::from_ref(left) as usize)
    }

    fn at(&self, address: usize) -> Option<&EnumComparisonState> {
        self.enums
            .binary_search_by_key(&address, |(address, _)| *address)
            .ok()
            .map(|index| &self.enums[index].1)
    }
}

type VisitRoots<T> = fn(&T, &mut dyn FnMut(&ScalarExpr));

/// Retains both the immutable source and its call state. Argument vectors and
/// binary operands are heap allocations, so their identity survives moves. They cannot
/// be changed or freed while the state is usable; addresses are only lookup keys
/// and are never dereferenced. Cloning prepares independent, initially empty state.
pub(crate) struct PreparedExpressions<T> {
    expressions: T,
    calls: Option<Box<PreparedCallStates<T>>>,
}

struct PreparedCallStates<T> {
    calls: FunctionCallStates,
    visit_roots: VisitRoots<T>,
}

static EMPTY_CALL_STATES: FunctionCallStates = FunctionCallStates { enums: Vec::new() };

impl<T> PreparedExpressions<T> {
    pub(crate) fn new(expressions: T, visit_roots: VisitRoots<T>) -> Self {
        let mut calls = FunctionCallStates::default();
        visit_roots(&expressions, &mut |expression| calls.register(expression));
        calls.enums.sort_unstable_by_key(|(address, _)| *address);
        calls.enums.dedup_by_key(|(address, _)| *address);
        Self {
            expressions,
            calls: (!calls.enums.is_empty())
                .then(|| Box::new(PreparedCallStates { calls, visit_roots })),
        }
    }

    pub(crate) fn calls(&self) -> &FunctionCallStates {
        self.calls
            .as_ref()
            .map_or(&EMPTY_CALL_STATES, |calls| &calls.calls)
    }
}

impl PreparedExpressions<ScalarExpr> {
    pub(crate) fn scalar(expression: ScalarExpr) -> Self {
        Self::new(expression, |expression, visit| visit(expression))
    }
}

impl PreparedExpressions<Vec<ScalarExpr>> {
    pub(crate) fn scalars(expressions: Vec<ScalarExpr>) -> Self {
        Self::new(expressions, |expressions, visit| {
            expressions.iter().for_each(visit);
        })
    }
}

impl PreparedExpressions<Vec<crate::SortKey>> {
    pub(crate) fn sort_keys(keys: Vec<crate::SortKey>) -> Self {
        Self::new(keys, |keys, visit| {
            for key in keys {
                visit(&key.expr);
            }
        })
    }
}

impl<T> std::ops::Deref for PreparedExpressions<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.expressions
    }
}

impl<T: Clone> Clone for PreparedExpressions<T> {
    fn clone(&self) -> Self {
        match self.calls.as_ref() {
            Some(calls) => Self::new(self.expressions.clone(), calls.visit_roots),
            None => Self {
                expressions: self.expressions.clone(),
                calls: None,
            },
        }
    }
}

#[cfg(test)]
mod tests;
