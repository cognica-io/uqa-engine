//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar evaluation over the shared SQL IR.

mod call_arguments;
mod context;
mod evaluator;
mod subquery;

pub use uqa_sql::ir::{
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec, SubqueryId,
};

pub use call_arguments::{
    eval_call_arguments, scalar_call_argument, scalar_call_arguments,
    validate_scalar_call_arguments, ScalarCallArgument,
};
pub use context::ScalarEvalContext;
pub use evaluator::eval_scalar;
pub(crate) use evaluator::scalar_integer_binary_width;
pub use subquery::{ScalarSubqueryRunner, SubqueryResult};

#[cfg(test)]
mod tests;
