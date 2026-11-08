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
        let value = owner.unified_value()?;
        Ok(Self { value, owner })
    }
}

impl Owner {
    pub(super) fn unified_value(&mut self) -> Result<UnifiedPlan, SQLError> {
        Ok(match &mut self.root {
            Node::Query(query) => {
                self.memory.grow(size_of::<QueryPlan>())?;
                let mut value = Box::new(empty_query());
                std::mem::swap(query, value.as_mut());
                UnifiedPlan::Query(value)
            }
            Node::Command(command) => {
                self.memory.grow(size_of::<CommandPlan>())?;
                let mut value = Box::new(CommandPlan::Empty);
                std::mem::swap(command, value.as_mut());
                UnifiedPlan::Command(value)
            }
            _ => unreachable!("bound body query or mutation"),
        })
    }

    pub(super) fn exchange_unified(&mut self, value: &mut UnifiedPlan) {
        match (value, &mut self.root) {
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

impl Deref for CapturedPlan {
    type Target = UnifiedPlan;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl Drop for CapturedPlan {
    fn drop(&mut self) {
        // The native outer box is left empty before normal field destruction; its original subtree returns to the admitted retirement workspace.
        self.owner.exchange_unified(&mut self.value);
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
