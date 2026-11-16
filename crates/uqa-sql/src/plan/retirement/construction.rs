//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partial constructors share the existing plan-tree retirement workspace. Structural descent admits its depth before a child can exist; sibling prefixes reuse that workspace when they retire.

use super::{Node, RetainedTree, Retire, RetirementFrame};
use crate::plan::{
    CommandPlan, CtePlan, DeletePlan, ExpressionPlan, InsertPlan, MergePlan, QueryBlockPlan,
    QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan, UpdatePlan,
};
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};
use uqa_core::memory::{BudgetedVec, MemoryError, Produced, ProductionControl};

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct Frame {
    node: Node,
    position: usize,
    retiring: bool,
}

impl RetirementFrame for Frame {
    type Node = Node;
    fn empty() -> Node {
        Node::Empty
    }
    fn is_empty(node: &Node) -> bool {
        matches!(node, Node::Empty)
    }
    fn new(node: Node) -> Self {
        Self {
            node,
            position: 0,
            retiring: false,
        }
    }
    fn begin_retirement(&mut self) {
        if !self.retiring {
            self.retiring = true;
            self.node.retire_scalars();
        }
    }
    fn retire_child(&mut self) -> Option<Node> {
        self.node.retire_child(&mut self.position)
    }
}

pub(in crate::plan) struct Workspace(RetainedTree<Frame>);

impl Workspace {
    fn node(&mut self, node: Node) {
        debug_assert!(matches!(self.0.root, Node::Empty));
        self.0.root = node;
        self.0.retire();
    }
}

#[derive(Clone)]
pub(in crate::plan) struct Context {
    workspace: Arc<Produced<Mutex<Workspace>>>,
    depth: usize,
}

impl Context {
    pub(in crate::plan) fn new(control: &ProductionControl<'_>) -> Result<Self, crate::SQLError> {
        control.check()?;
        let tree = RetainedTree::new(control.budget().expect("controlled plan construction"));
        Ok(Self {
            workspace: control
                .finish(Mutex::new(Workspace(tree)), control.empty_reservation())?
                .into_shared(control)?,
            depth: 0,
        })
    }

    pub(in crate::plan) fn child(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, crate::SQLError> {
        control.check()?;
        let depth = self.depth.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        // Required but still-empty boxed children can add one retirement level to a partial node.
        let capacity = depth.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        self.workspace
            .lock()
            .expect("plan retirement workspace")
            .0
            .frames
            .reserve(capacity)?;
        Ok(Self {
            workspace: self.workspace.clone(),
            depth,
        })
    }

    pub(in crate::plan) fn retain<T: Root>(&self, value: T) -> Prefix<T> {
        Prefix {
            value: Some(value),
            context: self.clone(),
        }
    }
}

pub(in crate::plan) trait Root {
    fn retire(self, workspace: &mut Workspace);
}

pub(in crate::plan) struct Prefix<T: Root> {
    value: Option<T>,
    context: Context,
}

impl<T: Root> Prefix<T> {
    pub(in crate::plan) fn take(mut self) -> T {
        self.value.take().expect("pending plan root")
    }
}
impl<T: Root> Deref for Prefix<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value.as_ref().expect("pending plan root")
    }
}
impl<T: Root> DerefMut for Prefix<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value.as_mut().expect("pending plan root")
    }
}
impl<T: Root> Drop for Prefix<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            value.retire(
                &mut self
                    .context
                    .workspace
                    .lock()
                    .expect("plan retirement workspace"),
            );
        }
    }
}

macro_rules! root {
    ($ty:ty, $variant:ident) => {
        impl Root for $ty {
            fn retire(self, workspace: &mut Workspace) {
                workspace.node(Node::$variant(self));
            }
        }
    };
}
root!(QueryPlan, Query);
root!(SourcePlan, Source);
root!(CtePlan, Cte);
root!(CommandPlan, Command);
root!(ExpressionPlan, Expression);
impl Root for UnifiedPlan {
    fn retire(self, workspace: &mut Workspace) {
        match self {
            Self::Query(value) => Root::retire(value, workspace),
            Self::Command(value) => Root::retire(value, workspace),
        }
    }
}
macro_rules! mutation_root {
    ($ty:ty, $child:ident) => {
        impl Root for $ty {
            fn retire(mut self, workspace: &mut Workspace) {
                Retire::retire(&mut self);
                while let Some(child) = super::commands::$child(&mut self) {
                    workspace.node(child);
                }
            }
        }
    };
}
mutation_root!(InsertPlan, insert_child);
mutation_root!(UpdatePlan, update_child);
mutation_root!(DeletePlan, delete_child);
impl Root for MergePlan {
    fn retire(mut self, workspace: &mut Workspace) {
        Retire::retire(&mut self);
        let mut position = 0;
        while let Some(child) = super::commands::merge_child(&mut self, &mut position) {
            workspace.node(child);
        }
    }
}
impl Root for RelationalPlan {
    fn retire(self, workspace: &mut Workspace) {
        let mut query = super::empty_query();
        query.root = self;
        workspace.node(Node::Query(query));
    }
}
impl Root for QueryBlockPlan {
    fn retire(mut self, workspace: &mut Workspace) {
        Retire::retire(&mut self);
        while let Some(child) = super::block_child(&mut self) {
            workspace.node(child);
        }
    }
}
impl<T: Root> Root for Box<T> {
    fn retire(self, workspace: &mut Workspace) {
        (*self).retire(workspace);
    }
}
impl<T: Root> Root for Option<T> {
    fn retire(self, workspace: &mut Workspace) {
        if let Some(value) = self {
            value.retire(workspace);
        }
    }
}
impl<T: Root> Root for Vec<T> {
    fn retire(self, workspace: &mut Workspace) {
        for value in self {
            value.retire(workspace);
        }
    }
}
impl<T: Root> Root for BudgetedVec<T> {
    fn retire(self, workspace: &mut Workspace) {
        let (values, lease) = self.into_parts();
        values.retire(workspace);
        drop(lease);
    }
}
