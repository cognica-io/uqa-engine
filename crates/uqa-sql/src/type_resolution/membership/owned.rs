//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected membership construction moves independently owned operands into admitted comparison containers.

use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch},
    plan::{OwnedScalarExpr, ScalarInputWork as Work},
    schema::retention::CatalogRetentionError,
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

#[cfg(test)]
mod tests;

struct Items {
    values: std::vec::IntoIter<OwnedScalarExpr>,
    _memory: Option<MemoryReservation>,
}

impl Items {
    fn new(value: Produced<Vec<OwnedScalarExpr>>) -> Self {
        let (values, memory) = value.into_parts();
        Self {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}

impl Iterator for Items {
    type Item = OwnedScalarExpr;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for Items {}

fn push(
    output: &mut ProductionVec<'_, OwnedScalarExpr>,
    value: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    Ok(output.push_produced(control.finish(value, control.empty_reservation())?)?)
}

async fn left<W: Work>(
    value: &mut Option<OwnedScalarExpr>,
    copy: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<OwnedScalarExpr, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    if copy {
        return value
            .as_ref()
            .expect("membership left operand")
            .copy_for_rewrite_from(control, work)
            .await;
    }
    Ok(value
        .take()
        .expect("final comparison moves the left operand"))
}

pub(crate) fn rewrite_owned(
    expression: OwnedScalarExpr,
    positions: &[usize],
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(crate::catalog::work::inline(rewrite_owned_from(
        expression,
        positions,
        control,
        &mut (),
    ))?)
}

pub(crate) async fn rewrite_owned_from<W: Work>(
    expression: OwnedScalarExpr,
    positions: &[usize],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<OwnedScalarExpr, W::Error>
where
    W::Error: From<CatalogRetentionError> + From<SQLError>,
{
    let ScalarExpr::InList { negated, .. } = &*expression else {
        return Err(
            SQLError::Internal("retained membership no longer matches its syntax".into()).into(),
        );
    };
    let negated = *negated;
    let mut children = expression.into_children();
    let mut value = Some(
        children
            .next_from(control, work)
            .await?
            .expect("membership left operand"),
    );
    let mut array = ProductionVec::new(*control);
    let mut individual = ProductionVec::new(*control);
    let mut selected = positions.iter().peekable();
    let mut index = 0;
    while let Some(item) = children.next_from(control, work).await? {
        let destination = if selected.peek() == Some(&&index) {
            selected.next();
            &mut array
        } else {
            &mut individual
        };
        push(destination, item, control).map_err(W::Error::from)?;
        index += 1;
    }
    drop(selected);
    let array = array.finish()?;
    let individual = individual.finish()?;
    let count = usize::from(!array.is_empty()) + individual.len();
    let mut comparisons = ProductionVec::new(*control);
    comparisons.reserve(count)?;
    if array.is_empty() {
        drop(array);
    } else {
        let lhs = left(&mut value, count > 1, control, work).await?;
        let array = OwnedScalarExpr::array_from(Items::new(array), control, work).await?;
        let (operator, memory) = control
            .copy_text(if negated { "<>" } else { "=" })?
            .into_parts();
        let operator =
            OwnedScalarExpr::literal(control.finish(Value::Str(operator), memory)?, control)?;
        let dispatch = if negated {
            FunctionDispatch::AllOperator
        } else {
            FunctionDispatch::AnyOperator
        };
        let binding = FunctionBinding::dispatched_with_control(dispatch, control)?;
        let call = OwnedScalarExpr::call_from(
            control.copy_text(if negated { "__all_op" } else { "__any_op" })?,
            Some(binding),
            [lhs, array, operator].into_iter(),
            control,
            control,
            work,
        )
        .await?
        .set_call_order_syntax(crate::ast::FunctionOrderSyntax::default(), control)?;
        push(&mut comparisons, call, control).map_err(W::Error::from)?;
    }
    let length = individual.len();
    for (index, item) in Items::new(individual).enumerate() {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        let lhs = left(&mut value, index + 1 < length, control, work).await?;
        let comparison = OwnedScalarExpr::binary(
            if negated {
                BinaryOp::NotEqual
            } else {
                BinaryOp::Equal
            },
            lhs,
            item,
            control,
        )?;
        push(&mut comparisons, comparison, control).map_err(W::Error::from)?;
    }
    if let Some(value) = value {
        value.retire_from(control, work).await?;
    }
    let mut comparisons = Items::new(comparisons.finish()?);
    let output = if comparisons.len() == 1 {
        let output = comparisons.next().expect("one comparison");
        drop(comparisons);
        output
    } else {
        OwnedScalarExpr::boolean_from(comparisons, negated, control, control, work).await?
    };
    Ok(output)
}
