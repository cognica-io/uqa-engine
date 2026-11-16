//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete SQL-to-execution-plan lowering.
//!
//! `OperatorTree` is the specialised algebra
//! for posting-list, graph, and fusion operations.  It is intentionally not a
//! relational algebra: forcing a SQL window frame or a mutation into a
//! posting-list node would erase its row and command semantics.  This module
//! supplies the missing super-plan.  Every SQL statement lowers to one
//! [`UnifiedPlan`], while query-producing statements recursively own their
//! relational children.  A physical driver can therefore use an
//! `OperatorTree` as an access path *inside* a relational node without keeping
//! a second top-level SQL dispatcher.

use crate::ast::{
    Expr, FrameBound, FromClause, NullsOrder, OrderBy, Projection, SelectStmt, SetOpKind,
    Statement, WindowSpec, CTE,
};
use crate::ir::{ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec};

mod analyzed;
mod command_children;
pub use analyzed::SharedAnalyzedSource;
mod command_expressions;
pub(crate) mod copy;
pub mod input_projection;
mod memory;
mod model;
mod mutable;
pub use mutable::OwnedScopedPlanVisitor;
pub use mutable::{
    CommandAssignmentSlot, CommandQuerySlot, CommandScalarSlot, MutableCommandPlan,
    MutableExecutablePlan, MutableExpressionPlan, MutableJoinParts, MutableJoinShell,
    MutableQueryPlan, MutableSourcePlan, OwnedDeferredRewriteIter, OwnedProjection,
    OwnedProjectionIter, OwnedValueRow, QueryChildSlot, QueryScalarSlot, ScalarArenaTransform,
    ScalarQueryArena, SourceChildSlot, SourceScalarSlot,
};
pub(crate) use mutable::{QueryMetadataUpdate, SourceMetadataUpdate};
mod optimization;
pub use optimization::ExecutablePlanOptimizer;
mod owned;
pub use owned::{ExecutableMemory, OwnedExecutablePlan, OwnedQueryPlan, SharedExecutablePlan};
mod query;
mod query_visit;
mod retained;
pub use retained::RetainedCtePlan;
mod retained_arena;
pub use retained_arena::RetainedQueryArena;
mod rewrite;
mod scalar;
pub use scalar::{OwnedScalarChildren, OwnedScalarExpr, ScalarNodeTransform};
mod scoped_visit;
pub use scoped_visit::ScopedPlanVisitor;
mod source_metadata;
mod source_visit;
mod statement;
pub mod subqueries;
mod visit;
pub mod windows;

pub use model::*;
pub use rewrite::rewrite_scalar_expression;

#[cfg(test)]
mod tests;

mod projection;
pub use projection::ProjectionTarget;

pub mod source_projection;

pub mod locking;
