//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained row lowering share destination admission and scalar ownership transfer.

use super::{Copying, QueryArena, Result};
use crate::{
    ast::Expr,
    plan::{
        mutable::memory::{RewriteField, RewriteMemory},
        retirement::Pending,
        AggregateClassifier, OwnedScalarExpr,
    },
    ScalarExpr,
};
use std::future::{ready, Future};
use uqa_core::memory::{BudgetedVec, ProductionControl};

pub(in crate::plan) trait RowInput<'s> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send;
    fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send;
}

struct InlineRows<'a, 'c> {
    aggregates: &'a dyn AggregateClassifier,
    arena: &'a mut QueryArena<'c>,
}

impl<'s> RowInput<'s> for InlineRows<'_, '_> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<()>> + Send {
        ready(Ok(()))
    }

    fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> impl Future<Output = Result<OwnedScalarExpr>> + Send {
        ready(
            crate::plan::scalar::lower_scalar_with_arena(
                expression,
                self.aggregates,
                self.arena,
                control,
            )
            .map_err(Into::into),
        )
    }
}

impl Copying<'_> {
    pub(super) fn lower_rows(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<Vec<ScalarExpr>>> {
        crate::catalog::work::inline(construct_rows(
            rows,
            &mut InlineRows { aggregates, arena },
            self.control,
            self.fields.as_mut().expect("row records"),
            self.memory.as_mut().expect("row allowance"),
        ))
    }
}

async fn construct_rows<'s, I: RowInput<'s>>(
    rows: &'s [Vec<Expr>],
    input: &mut I,
    control: ProductionControl<'_>,
    fields: &mut BudgetedVec<RewriteField>,
    direct: &mut uqa_core::memory::MemoryReservation,
) -> Result<Vec<Vec<ScalarExpr>>> {
    control.check()?;
    let budget = control.budget().expect("controlled row destination");
    let mut output = Pending::new(BudgetedVec::new(budget));
    output.reserve(rows.len())?;
    fields.reserve(rows.len())?;
    for row in rows {
        input.checkpoint().await?;
        control.check()?;
        let mut owners = BudgetedVec::new(budget);
        let mut values = Pending::new(BudgetedVec::new(budget));
        owners.reserve(row.len())?;
        values.reserve(row.len())?;
        for expression in row {
            input.checkpoint().await?;
            control.check()?;
            let scalar = input.scalar(expression, control).await?;
            let scalar = scalar.rewrite(&control, Ok::<_, crate::SQLError>)?;
            let (value, memory) = scalar.into_rewrite_parts();
            values.push(value).expect("admitted scalar destination");
            owners.push(memory).expect("admitted scalar owner");
        }
        let (values, reservation) = values.take().into_parts();
        output.push(values).expect("admitted row destination");
        fields
            .push(RewriteField {
                name: "item",
                memory: Some(RewriteMemory::Scalars {
                    direct: reservation,
                    items: owners,
                }),
            })
            .expect("admitted row owner");
    }
    let (output, reservation) = output.take().into_parts();
    direct.absorb(reservation);
    Ok(output)
}

#[cfg(test)]
mod tests;
