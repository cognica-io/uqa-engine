//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, ExpressionPlan, Fields, Input, Kind, Node, Owner, QueryPlan, SQLError};
use std::{io::Read, ops::Deref};
use uqa_core::{image_io::ImageDecoder, memory::MemoryBudget, CancellationToken};

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
    /// Retain the original native plan, partial fields and query/window addresses across independent byte and continuation-poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<CapturedQuery, SQLError>, SQLError> {
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                let mut retained_children = 0;
                let mut input = Fields {
                    binary: reader.input(),
                    retained_children: &mut retained_children,
                };
                if input
                    .header(super::super::super::expressions::plans::MAGIC)
                    .await?
                    != 1
                {
                    return Err(invalid("native query root count"));
                }
                let owner = input.native_plan(Kind::Query).await?;
                input.binary.end().await?;
                Ok(CapturedQuery(owner))
            },
        )?)
    }

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
    /// Retain the original native plan, partial fields and query/window addresses across independent byte and continuation-poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<CapturedExpressionPlan, SQLError>, SQLError> {
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move {
                let mut retained_children = 0;
                let mut input = Fields {
                    binary: reader.input(),
                    retained_children: &mut retained_children,
                };
                if input
                    .header(super::super::super::expressions::plans::EXPRESSION_MAGIC)
                    .await?
                    != 1
                {
                    return Err(invalid("native expression-plan root count"));
                }
                let owner = input.native_plan(Kind::Expression).await?;
                input.binary.end().await?;
                Ok(CapturedExpressionPlan(owner))
            },
        )?)
    }

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
