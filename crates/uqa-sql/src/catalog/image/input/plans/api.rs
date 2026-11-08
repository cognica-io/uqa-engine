//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, ExpressionPlan, Input, Kind, Node, Owner, QueryPlan, SQLError};
use std::{io::Read, ops::Deref};
use uqa_core::{memory::MemoryBudget, CancellationToken};

/// Immutable reconstructed query syntax with admitted payloads and non-recursive retirement. The owner retains its original decoder workspace for cleanup, including when the invoking allowance is exhausted.
#[derive(Debug)]
pub struct CapturedQuery(Owner);

impl Deref for CapturedQuery {
    type Target = QueryPlan;
    fn deref(&self) -> &Self::Target {
        let Node::Query(value) = &self.0.root else {
            unreachable!("captured query root")
        };
        value
    }
}

/// Immutable native expression and its original query arena under one reconstruction owner.
#[derive(Debug)]
pub struct CapturedExpressionPlan(Owner);

impl Deref for CapturedExpressionPlan {
    type Target = ExpressionPlan;
    fn deref(&self) -> &Self::Target {
        let Node::Expression(value) = &self.0.root else {
            unreachable!("captured expression root")
        };
        value
    }
}

impl QueryPlan {
    /// Reconstruct the native stored projection without parsing, executing or rebinding SQL. Every scalar address is validated against its owning query/window arena before publication.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<CapturedQuery, SQLError> {
        let mut input = Input::new(reader, memory, cancellation);
        if input.header(super::super::super::expressions::plans::MAGIC)? != 1 {
            return Err(invalid("native query root count"));
        }
        let owner = input.native_plan(Kind::Query)?;
        input.binary.end()?;
        Ok(CapturedQuery(owner))
    }
}

impl ExpressionPlan {
    /// Reconstruct an expression with its original ordered query arena and retained native ownership.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<CapturedExpressionPlan, SQLError> {
        let mut input = Input::new(reader, memory, cancellation);
        if input.header(super::super::super::expressions::plans::EXPRESSION_MAGIC)? != 1 {
            return Err(invalid("native expression-plan root count"));
        }
        let owner = input.native_plan(Kind::Expression)?;
        input.binary.end()?;
        Ok(CapturedExpressionPlan(owner))
    }
}
