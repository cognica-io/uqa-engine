//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored syntax shares native plan admission, suspended ownership and iterative retirement.

use super::{invalid, Input, PlanMemory, SQLError};
use crate::ast::{
    AssignmentStep, AssignmentTarget, AssignmentTargets, CteCycleClause, DeleteStmt, Expr,
    FrameBound, FromClause, InsertStmt, MergeStmt, MergeWhen, MultipleAssignmentTargets,
    OnConflict, OnConflictAction, OrderBy, Projection, SelectStmt, SetOp, Statement, TableFunction,
    UpdateStmt, WindowDefinition, WindowFrame, WindowSpec, CTE,
};
type Step = super::driver::Step<Kind>;

mod api;
pub use api::CapturedStatement;
mod fields;
use fields::{Body, Empty, Field, Tree};
#[macro_use]
mod schema;
mod expressions;
mod metadata;
mod mutations;
mod queries;
mod sources;
#[cfg(test)]
mod tests;
mod validation;

syntax_nodes! {
    Statement: Statement, Query: SelectStmt, Source: FromClause, Expression: Expr,
    Set: SetOp, Cte: CTE, Cycle: CteCycleClause,
    Insert: InsertStmt, Update: UpdateStmt, Delete: DeleteStmt, Merge: MergeStmt,
    Conflict: OnConflict, ConflictAction: OnConflictAction, MergeAction: MergeWhen,
    Target: AssignmentTarget, Targets: AssignmentTargets, Step: AssignmentStep<Expr>,
    MultipleTargets: MultipleAssignmentTargets<Expr>, Assignment: (AssignmentTargets, Expr),
    Projection: Projection, Order: OrderBy, Function: TableFunction,
    Window: WindowDefinition, WindowSpec: WindowSpec, WindowFrame: WindowFrame, Bound: FrameBound,
    Expressions: Vec<Expr>, Rows: Vec<Vec<Expr>>, Ctes: Vec<CTE>,
    Projections: Vec<Projection>, Orders: Vec<OrderBy>, Functions: Vec<TableFunction>,
    Windows: Vec<WindowDefinition>, Assignments: Vec<(AssignmentTargets, Expr)>,
    TargetsList: Vec<AssignmentTarget>, Steps: Vec<AssignmentStep<Expr>>, MergeActions: Vec<MergeWhen>,
}

syntax_members! {
    Statement, SelectStmt, FromClause, Expr, SetOp, CteCycleClause,
    InsertStmt, UpdateStmt, DeleteStmt, MergeStmt, OnConflict, OnConflictAction, MergeWhen,
    AssignmentTarget, AssignmentTargets, AssignmentStep<Expr>, MultipleAssignmentTargets<Expr>,
    (AssignmentTargets, Expr), Projection, OrderBy, TableFunction,
    WindowDefinition, WindowSpec, WindowFrame, FrameBound,
    Vec<Expr>, Vec<Vec<Expr>>, Vec<CTE>, Vec<Projection>, Vec<OrderBy>, Vec<TableFunction>,
    Vec<WindowDefinition>, Vec<(AssignmentTargets, Expr)>, Vec<AssignmentTarget>,
    Vec<AssignmentStep<Expr>>, Vec<MergeWhen>,
}

fn construct<T: Tree>(memory: &mut PlanMemory) -> Result<Node, SQLError> {
    Ok(<T as Body>::new(memory)?.into_node())
}

impl super::NativeNode for Node {
    type Kind = Kind;
    fn empty() -> Self {
        Self::Empty
    }
    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
    fn new(kind: Kind, memory: &mut PlanMemory) -> Result<Self, SQLError> {
        kind.begin(memory)
    }
    fn read(
        frame: &mut super::Frame<Self>,
        input: &mut Input<'_>,
        memory: &mut PlanMemory,
    ) -> Result<Step, SQLError> {
        frame
            .node
            .body()
            .read(frame.position, &mut frame.count, input, memory)
    }
    fn validate(frame: &mut super::Frame<Self>, input: &Input<'_>) -> Result<(), SQLError> {
        validation::observe(&frame.node, &mut frame.references);
        validation::validate(&frame.node, &frame.references, input)
    }
    fn attach(frame: &mut super::Frame<Self>, child: super::Frame<Self>) {
        if validation::carries_enclosing_references(&child.node) {
            frame.references.absorb(child.references);
        }
        frame.node.body().attach(&mut frame.position, child.node);
    }
    fn retire_scalars(_: &mut super::Frame<Self>) {}
    fn retire_child(frame: &mut super::Frame<Self>) -> Option<Self> {
        frame.node.body().retire(&mut frame.position)
    }
}
