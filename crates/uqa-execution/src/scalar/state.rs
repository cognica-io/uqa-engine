//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function state belongs to immutable expressions prepared for one execution.

use uqa_sql::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch},
    expr::enums::EnumComparisonState,
    ScalarExpr,
};

#[cfg(test)]
thread_local! {
    static COMPARISON_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Read-only lookup into the function state owned by a prepared expression set.
/// The state has one fixed-size entry per logical ordering comparison, never per input row.
#[derive(Debug, Default)]
pub struct FunctionCallStates {
    enums: Vec<((usize, u8), EnumComparisonState)>,
}

impl FunctionCallStates {
    fn register(&mut self, expression: &ScalarExpr) {
        expression.visit(&mut |expression| match expression {
            ScalarExpr::Func { name, binding, args, .. } => {
                self.add(args.as_ptr() as usize, function_slots(name, binding.as_ref(), args));
            }
            ScalarExpr::Binary {
                op: BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual,
                lhs, ..
            } => self.add(std::ptr::from_ref(lhs.as_ref()) as usize, 1),
            ScalarExpr::Between { low, high, .. } => {
                self.add(std::ptr::from_ref(low.as_ref()) as usize, 1);
                self.add(std::ptr::from_ref(high.as_ref()) as usize, 1);
            }
            _ => {},
        });
    }

    fn add(&mut self, address: usize, slots: u8) {
        for slot in 0..slots {
            self.enums
                .push(((address, slot), EnumComparisonState::default()));
        }
    }

    pub(crate) fn enum_comparisons(
        &self,
        arguments: &[ScalarExpr],
    ) -> [Option<&EnumComparisonState>; 4] {
        let address = arguments.as_ptr() as usize;
        let Ok(first) = self
            .enums
            .binary_search_by_key(&(address, 0), |(key, _)| *key)
        else {
            return [None; 4];
        };
        std::array::from_fn(|slot| {
            self.enums
                .get(first + slot)
                .filter(|(key, _)| *key == (address, slot as u8))
                .map(|(_, state)| state)
        })
    }

    pub(crate) fn enum_binary_comparison(&self, left: &ScalarExpr) -> Option<&EnumComparisonState> {
        #[cfg(test)]
        COMPARISON_LOOKUPS.with(|count| count.set(count.get() + 1));
        self.enums
            .binary_search_by_key(&(std::ptr::from_ref(left) as usize, 0), |(key, _)| *key)
            .ok()
            .map(|index| &self.enums[index].1)
    }
}

fn function_slots(name: &str, binding: Option<&FunctionBinding>, args: &[ScalarExpr]) -> u8 {
    if binding.is_some_and(|binding| !binding.builtin) {
        return 0;
    }
    match binding.and_then(|binding| binding.dispatch) {
        Some(FunctionDispatch::Enum { operation, .. }) => {
            u8::from(args.len() == 2 && operation.uses_comparison_state())
        }
        Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator) => u8::from(
            args.len() == 3
                && matches!(args.last(),
                Some(ScalarExpr::Literal(uqa_core::Value::Str(op)))
                    if matches!(op.as_str(), "<" | "<=" | ">" | ">=")),
        ),
        Some(FunctionDispatch::BetweenSymmetric) if args.len() == 3 => 4,
        None => u8::from(
            args.len() >= 2
                && (name.eq_ignore_ascii_case("greatest") || name.eq_ignore_ascii_case("least")),
        ),
        _ => 0,
    }
}

type VisitRoots<T> = fn(&T, &mut dyn FnMut(&ScalarExpr));

/// Retains both the immutable source and its call state. Argument vectors and
/// comparison operands are heap allocations, so their identity survives moves. They cannot
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
