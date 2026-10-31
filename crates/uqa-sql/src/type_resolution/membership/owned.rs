//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected membership construction moves independently owned operands into admitted comparison containers.

use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch},
    plan::OwnedScalarExpr,
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

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

fn left(
    value: &mut Option<OwnedScalarExpr>,
    copy: bool,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if copy {
        return Ok(value
            .as_ref()
            .expect("membership left operand")
            .copy_for_rewrite(control)?);
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
    let ScalarExpr::InList { negated, .. } = &*expression else {
        return Err(SQLError::Internal(
            "retained membership no longer matches its syntax".into(),
        ));
    };
    let negated = *negated;
    let mut children = expression.into_children();
    let mut value = Some(children.next().expect("membership left operand"));
    let mut array = ProductionVec::new(*control);
    let mut individual = ProductionVec::new(*control);
    let mut selected = positions.iter().peekable();
    for (index, item) in children.enumerate() {
        let destination = if selected.peek() == Some(&&index) {
            selected.next();
            &mut array
        } else {
            &mut individual
        };
        push(destination, item, control)?;
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
        let lhs = left(&mut value, count > 1, control)?;
        let array = OwnedScalarExpr::array(Items::new(array), control)?;
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
        let call = OwnedScalarExpr::call(
            control.copy_text(if negated { "__all_op" } else { "__any_op" })?,
            Some(binding),
            [lhs, array, operator].into_iter(),
            control,
        )?
        .set_call_order_syntax(crate::ast::FunctionOrderSyntax::default(), control)?;
        push(&mut comparisons, call, control)?;
    }
    let length = individual.len();
    for (index, item) in Items::new(individual).enumerate() {
        let lhs = left(&mut value, index + 1 < length, control)?;
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
        push(&mut comparisons, comparison, control)?;
    }
    drop(value);
    let mut comparisons = Items::new(comparisons.finish()?);
    let output = if comparisons.len() == 1 {
        let output = comparisons.next().expect("one comparison");
        drop(comparisons);
        output
    } else {
        OwnedScalarExpr::boolean(comparisons, negated, control)?
    };
    Ok(output)
}
