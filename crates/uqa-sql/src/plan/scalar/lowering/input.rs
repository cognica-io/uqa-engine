//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar construction suspends inside payloads and between children without recursive polling.

use super::{Control, Expr, Lowering, OwnedScalarExpr, ScalarExpr, ScalarMemory};
use crate::{plan::scalar::owned::child_slot_mut, SQLError};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{BudgetedVec, ProductionControl},
};

mod children;
mod nodes;

struct Frame<'s, 'c> {
    source: &'s Expr,
    // Retire partially installed nodes before returning their direct and child allowances.
    output: ScalarExpr,
    lowering: Lowering<'c>,
    next: usize,
}

impl Drop for Frame<'_, '_> {
    fn drop(&mut self) {
        crate::ir::ownership::retire(&mut self.output);
    }
}

impl<'s, 'c> Frame<'s, 'c> {
    async fn new(
        source: &'s Expr,
        control: ProductionControl<'c>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut lowering = Lowering {
            control: Some(Control::from_production(control)),
            owned_children: Some(BudgetedVec::new(
                control.budget().expect("native scalar allowance"),
            )),
        };
        let output = lowering.scalar_node_from(source, input).await?;
        Ok(Self {
            source,
            output,
            lowering,
            next: 0,
        })
    }

    fn finish(mut self) -> Result<OwnedScalarExpr, SQLError> {
        self.lowering.production().check()?;
        Ok(OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(&mut self.output, ScalarExpr::Star),
            Some(ScalarMemory::from_parts(
                self.lowering
                    .control
                    .take()
                    .expect("native scalar owner")
                    .memory,
                self.lowering
                    .owned_children
                    .take()
                    .expect("native scalar children"),
            )),
        ))
    }

    fn attach(&mut self, child: OwnedScalarExpr) -> Result<(), SQLError> {
        let value = OwnedScalarExpr::attach_child(&mut self.lowering, child)?;
        *child_slot_mut(&mut self.output, self.next - 1).expect("suspended scalar child") = value;
        Ok(())
    }
}

impl crate::plan::ExpressionPlan {
    /// Lower scalar-only retained syntax into independently movable children through the native work channel. Scalar descendants use admitted iterative frames; names, literal values and call metadata suspend within their own payload constructors. Query children preserve the scalar-only rejection boundary.
    pub async fn lower_scalar_from_input(
        expression: &Expr,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("native scalar construction requires a destination allowance".into())
        })?;
        let mut frames = BudgetedVec::new(memory);
        frames.reserve(1)?;
        frames.push(Frame::new(expression, *control, input).await?)?;
        loop {
            control.check()?;
            input.checkpoint().await?;
            control.check()?;
            let parent = frames.last_mut().expect("active scalar constructor");
            if let Some(child) = children::get(parent.source, parent.next) {
                parent.next += 1;
                parent
                    .lowering
                    .owned_children
                    .as_mut()
                    .expect("native scalar children")
                    .reserve(1)?;
                frames.reserve(1)?;
                frames.push(Frame::new(child, *control, input).await?)?;
            } else {
                let value = frames
                    .pop()
                    .expect("completed scalar constructor")
                    .finish()?;
                match frames.last_mut() {
                    Some(parent) => parent.attach(value)?,
                    None => return Ok(value),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
