//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered scalar vectors share suspended field admission with ordinary query construction.

use super::{Copying, Result, ScalarInput};
use crate::{
    ast::Expr,
    plan::retirement::{Pending, Retire},
    ScalarExpr,
};
use uqa_core::memory::BudgetedVec;

pub(in crate::plan::copy::lowering) async fn construct_scalars<
    's,
    T: Sync,
    U: Retire + Send,
    I: ScalarInput<&'s Expr>,
>(
    source: &'s [T],
    copying: &mut Copying<'_>,
    input: &mut I,
    expression: fn(&T) -> &Expr,
    construct: fn(&T, ScalarExpr) -> U,
) -> Result<Pending<Vec<U>>> {
    copying.check()?;
    input.checkpoint().await?;
    copying.check()?;
    let budget = copying.control.budget().expect("controlled scalar fields");
    let mut values = Pending::new(BudgetedVec::new(budget));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("scalar fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let scalar = input.scalar(expression(source), copying.control).await?;
        let scalar = field.take_scalar(scalar)?;
        values
            .push(construct(source, scalar))
            .expect("admitted scalar field");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}
