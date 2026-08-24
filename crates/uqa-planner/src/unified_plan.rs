//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete SQL-to-execution-plan lowering.
//!
//! [`OperatorTree`](uqa_operators::OperatorTree) is the specialised algebra
//! for posting-list, graph, and fusion operations.  It is intentionally not a
//! relational algebra: forcing a SQL window frame or a mutation into a
//! posting-list node would erase its row and command semantics.  This module
//! supplies the missing super-plan.  Every SQL statement lowers to one
//! [`UnifiedPlan`], while query-producing statements recursively own their
//! relational children.  A physical driver can therefore use an
//! `OperatorTree` as an access path *inside* a relational node without keeping
//! a second top-level SQL dispatcher.

use uqa_execution::{
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};
use uqa_sql::ast::{
    Expr, FrameBound, FromClause, NullsOrder, OrderBy, Projection, SelectStmt, SetOpKind,
    Statement, WindowSpec, CTE,
};

mod model;
mod query;
mod rewrite;
mod scalar;
mod statement;

pub use model::*;
pub use rewrite::rewrite_scalar_expression;
pub(crate) use scalar::is_builtin_aggregate;

#[cfg(test)]
mod tests;
