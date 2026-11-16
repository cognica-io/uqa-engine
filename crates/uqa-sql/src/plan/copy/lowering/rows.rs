//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained row lowering share destination admission and scalar ownership transfer.

use super::{
    input::{InlineScalarInput, ScalarInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::Expr,
    plan::{
        mutable::memory::{RewriteField, RewriteMemory},
        retirement::Pending,
        AggregateClassifier,
    },
    ScalarExpr,
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

impl Copying<'_> {
    pub(super) fn lower_rows(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<Vec<Vec<ScalarExpr>>> {
        crate::catalog::work::inline(construct_rows(
            rows,
            &mut InlineScalarInput { aggregates, arena },
            self.control,
            self.fields.as_mut().expect("row records"),
            self.memory.as_mut().expect("row allowance"),
        ))
    }
}

async fn construct_rows<'s, I: ScalarInput<'s>>(
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
