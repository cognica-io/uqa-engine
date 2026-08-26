//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational Volcano operators.
//!
//! Each operator family owns its state and lifecycle in a focused module while
//! this facade preserves the public construction API.

use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::SetOpKind;
use uqa_sql::expr::truthy;
#[cfg(test)]
use uqa_sql::ResultRow;
use uqa_sql::SQLParam;

use crate::batch::{Batch, RowSchema};
use crate::physical::{ExecError, ExecResult, PhysicalOperator};
use crate::scalar::{eval_scalar, ScalarEvalContext, ScalarExpr};

mod aggregate;
mod evaluator;
mod filter;
mod limit;
mod project;
mod set_operation;
mod sort;
mod window;

pub use aggregate::{AggregateExecutor, AggregateKind, AggregateSpec, HashAggregate};
pub use evaluator::{
    ExpressionEvaluator, RowPredicate, SharedExpressionEvaluator, SharedRowPredicate,
};
pub use filter::Filter;
pub use limit::Limit;
pub use project::{Project, ProjectionTarget};
pub use set_operation::SetOperation;
pub(crate) use sort::compare_sort_key_values_by;
pub use sort::{compare_sort_key_values, Sort, SortKey};
pub use window::{Window, WindowExecutor, WindowKind, WindowSpec};

use aggregate::value_to_f64;
use evaluator::DefaultExpressionEvaluator;
use sort::compare_values;

#[cfg(test)]
use aggregate::{finalise_fold, AggFold};

#[cfg(test)]
mod tests;
