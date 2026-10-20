//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function state belongs to immutable expressions prepared for one execution.

use std::sync::OnceLock;

use crate::RowSchema;

use uqa_core::{memory::Produced, Value};
use uqa_sql::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch},
    expr::enums::EnumComparisonState,
    ScalarExpr,
};

#[cfg(test)]
thread_local! {
    static COMPARISON_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CAST_TYPE_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static COLUMN_NAME_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The first conversion of a cast whose operand is a literal.
#[derive(Debug)]
pub(crate) enum ConstantCast {
    /// An immutable conversion, which every later evaluation copies.
    Retained(Value),
    /// The conversion reads a catalog descriptor, a session setting or a type outside the builtin scalars, so every evaluation converts again.
    PerEvaluation,
}

/// Selected builtin scalar conversions are retained for the same immutable execution schema; catalog-defined and session-dependent casts keep their live path.
#[derive(Debug)]
pub(crate) enum RowCast {
    Resolved {
        conversion: uqa_sql::expr::ResolvedCast,
        source: Produced<String>,
        schema: Option<RowSchema>,
    },
    PerEvaluation,
}

/// A fixed numeric overload tied to the immutable schema that selected it.
#[derive(Debug)]
pub(crate) enum NumericCall {
    Resolved {
        operation: uqa_sql::expr::ResolvedNumericOperator,
        schema: Option<RowSchema>,
    },
    PerEvaluation,
}

/// The context schema the resolution of a column reference depended on.
#[derive(Debug)]
pub(crate) enum ContextSchema {
    /// A qualified reference never consults the context schema.
    Unused,
    /// The context carried no schema.
    Absent,
    /// The context carried this schema, which decided between a column and a whole-row reference.
    Shared(RowSchema),
}

/// The key a column reference reads once the schema of its rows is known.
#[derive(Debug)]
pub(crate) struct ColumnSlot {
    pub(crate) context_schema: ContextSchema,
    /// The schema the rows resolved the reference through. Retaining it keeps the index alive, so a later schema can never reuse its address.
    pub(crate) row_schema: RowSchema,
    /// The space of `key`: rows of the same schema read through a lookup of another space resolve the name again.
    pub(crate) key_space: uqa_sql::expr::RowKeySpace,
    /// The row lookup's key for the reference, or `None` for a name the rows do not carry.
    pub(crate) key: Option<usize>,
}

/// Read-only lookup into the function state owned by a prepared expression set.
/// The state has one fixed-size entry per logical ordering comparison, numeric operator, literal/row cast or column reference, never per input row.
#[derive(Debug, Default)]
pub struct FunctionCallStates {
    enums: Vec<((usize, u8), EnumComparisonState)>,
    constant_casts: Vec<(usize, OnceLock<ConstantCast>)>,
    columns: Vec<(usize, OnceLock<ColumnSlot>)>,
    row_casts: Vec<(usize, OnceLock<RowCast>)>,
    numeric_operators: Vec<(usize, OnceLock<NumericCall>)>,
}

impl FunctionCallStates {
    fn register(&mut self, expression: &ScalarExpr) {
        let first_row_cast = self.row_casts.len();
        let first_numeric = self.numeric_operators.len();
        let mut parameter_dependent = false;
        expression.visit(&mut |expression| match expression {
            ScalarExpr::Func { name, binding, args, .. } => {
                self.add(args.as_ptr() as usize, function_slots(name, binding.as_ref(), args));
                if binding.as_ref().is_some_and(|binding| binding.builtin
                    && matches!(binding.dispatch, Some(FunctionDispatch::NumericOperator(operator)) if args.len() == operator.arity())) {
                    self.numeric_operators.push((args.as_ptr() as usize, OnceLock::new()));
                }
            }
            ScalarExpr::Binary {
                op: BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual,
                lhs, ..
            } => self.add(std::ptr::from_ref(lhs.as_ref()) as usize, 1),
            ScalarExpr::Between { low, high, .. } => {
                self.add(std::ptr::from_ref(low.as_ref()) as usize, 1);
                self.add(std::ptr::from_ref(high.as_ref()) as usize, 1);
            }
            ScalarExpr::Cast { expr, .. }
                if matches!(expr.as_ref(), ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }) =>
            {
                self.constant_casts
                    .push((std::ptr::from_ref(expr.as_ref()) as usize, OnceLock::new()));
            }
            ScalarExpr::Cast { expr, .. } => {
                self.row_casts.push((std::ptr::from_ref(expr.as_ref()) as usize, OnceLock::new()));
            }
            ScalarExpr::Param(_) => parameter_dependent = true,
            ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => {
                self.columns
                    .push((std::ptr::from_ref(expression) as usize, OnceLock::new()));
            }
            _ => {},
        });
        // One traversal per root avoids rescanning every nested cast's subtree. Parameter-dependent roots keep live inference without disabling independent roots.
        if parameter_dependent {
            self.row_casts.truncate(first_row_cast);
            self.numeric_operators.truncate(first_numeric);
        }
    }

    fn is_empty(&self) -> bool {
        self.enums.is_empty()
            && self.constant_casts.is_empty()
            && self.columns.is_empty()
            && self.row_casts.is_empty()
            && self.numeric_operators.is_empty()
    }

    /// The retained key of the column reference `expression`.
    pub(crate) fn column_slot(&self, expression: &ScalarExpr) -> Option<&OnceLock<ColumnSlot>> {
        let address = std::ptr::from_ref(expression) as usize;
        self.columns
            .binary_search_by_key(&address, |(key, _)| *key)
            .ok()
            .map(|index| &self.columns[index].1)
    }

    /// The retained first conversion of the cast whose literal operand is `operand`.
    pub(crate) fn constant_cast(&self, operand: &ScalarExpr) -> Option<&OnceLock<ConstantCast>> {
        let address = std::ptr::from_ref(operand) as usize;
        self.constant_casts
            .binary_search_by_key(&address, |(key, _)| *key)
            .ok()
            .map(|index| &self.constant_casts[index].1)
    }

    pub(crate) fn row_cast(&self, operand: &ScalarExpr) -> Option<&OnceLock<RowCast>> {
        let address = std::ptr::from_ref(operand) as usize;
        self.row_casts
            .binary_search_by_key(&address, |(key, _)| *key)
            .ok()
            .map(|index| &self.row_casts[index].1)
    }

    pub(crate) fn numeric_operator(
        &self,
        arguments: &[ScalarExpr],
    ) -> Option<&OnceLock<NumericCall>> {
        let address = arguments.as_ptr() as usize;
        self.numeric_operators
            .binary_search_by_key(&address, |(key, _)| *key)
            .ok()
            .map(|index| &self.numeric_operators[index].1)
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

static EMPTY_CALL_STATES: FunctionCallStates = FunctionCallStates {
    enums: Vec::new(),
    constant_casts: Vec::new(),
    columns: Vec::new(),
    row_casts: Vec::new(),
    numeric_operators: Vec::new(),
};

impl<T> PreparedExpressions<T> {
    pub(crate) fn new(expressions: T, visit_roots: VisitRoots<T>) -> Self {
        let mut calls = FunctionCallStates::default();
        visit_roots(&expressions, &mut |expression| calls.register(expression));
        calls.enums.sort_unstable_by_key(|(address, _)| *address);
        calls.enums.dedup_by_key(|(address, _)| *address);
        calls
            .constant_casts
            .sort_unstable_by_key(|(address, _)| *address);
        calls.constant_casts.dedup_by_key(|(address, _)| *address);
        calls.columns.sort_unstable_by_key(|(address, _)| *address);
        calls.columns.dedup_by_key(|(address, _)| *address);
        calls
            .row_casts
            .sort_unstable_by_key(|(address, _)| *address);
        calls.row_casts.dedup_by_key(|(address, _)| *address);
        calls
            .numeric_operators
            .sort_unstable_by_key(|(address, _)| *address);
        calls
            .numeric_operators
            .dedup_by_key(|(address, _)| *address);
        Self {
            expressions,
            calls: (!calls.is_empty()).then(|| Box::new(PreparedCallStates { calls, visit_roots })),
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
