//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A bound routine member retains the same iterative owner as its native query or command.

use super::{empty_query, invalid, CommandPlan, Input, Kind, Node, Owner, QueryPlan, SQLError};
use crate::plan::UnifiedPlan;
use std::{io::Read, ops::Deref};
use uqa_core::{memory::MemoryBudget, CancellationToken};

/// Immutable reconstructed query or mutation command with admitted original fields and iterative full/partial retirement.
#[derive(Debug)]
pub struct CapturedPlan {
    value: UnifiedPlan,
    owner: Owner,
}

impl CapturedPlan {
    fn new(mut owner: Owner) -> Result<Self, SQLError> {
        let value = match &mut owner.root {
            Node::Query(query) => {
                owner.memory.grow(size_of::<QueryPlan>())?;
                let mut value = Box::new(empty_query());
                std::mem::swap(query, value.as_mut());
                UnifiedPlan::Query(value)
            }
            Node::Command(command) => {
                owner.memory.grow(size_of::<CommandPlan>())?;
                let mut value = Box::new(CommandPlan::Empty);
                std::mem::swap(command, value.as_mut());
                UnifiedPlan::Command(value)
            }
            _ => unreachable!("bound body query or mutation"),
        };
        Ok(Self { value, owner })
    }
}

impl Deref for CapturedPlan {
    type Target = UnifiedPlan;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl Drop for CapturedPlan {
    fn drop(&mut self) {
        // The native outer box is left empty before normal field destruction; its original subtree returns to the admitted retirement workspace.
        match (&mut self.value, &mut self.owner.root) {
            (UnifiedPlan::Query(value), Node::Query(query)) => {
                std::mem::swap(value.as_mut(), query);
            }
            (UnifiedPlan::Command(value), Node::Command(command)) => {
                std::mem::swap(value.as_mut(), command);
            }
            _ => unreachable!("immutable bound body owner"),
        }
    }
}

impl UnifiedPlan {
    /// Reconstruct a stored bound SQL body member without lowering, resolving or executing it. Query/window addresses and every mutation's private arena retain the existing native validation.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<CapturedPlan, SQLError> {
        let mut input = Input::new(reader, memory, cancellation);
        if input.header(super::super::super::expressions::plans::UNIFIED_MAGIC)? != 1 {
            return Err(invalid("bound SQL plan root count"));
        }
        let kind = match input.binary.array::<1>()?[0] {
            0 => Kind::Query,
            1 => Kind::Command,
            _ => return Err(invalid("bound SQL plan root kind")),
        };
        let owner = input.native_plan(kind)?;
        input.binary.end()?;
        CapturedPlan::new(owner)
    }
}

#[cfg(test)]
mod tests;
