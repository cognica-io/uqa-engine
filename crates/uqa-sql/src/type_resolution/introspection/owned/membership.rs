//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IN-list analysis transfers each child into its selected comparison and retires discarded parent storage immediately.

use super::{ColumnType, OwnedBinder, OwnedScalarExpr, SQLError, ScalarExpr};
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch},
    type_resolution::{common::unknown_input, OwnedBindingCall},
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

impl OwnedBinder<'_> {
    pub(super) fn coerce_membership(
        &self,
        mut expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let ScalarExpr::InList {
            expr,
            list,
            negated,
        } = &*expression
        else {
            unreachable!()
        };
        let negated = *negated;
        let control = &self.context.control;
        for operand in std::iter::once(expr.as_ref()).chain(list.iter()) {
            if !unknown_input(operand)
                && self
                    .context
                    .semantic(self.context.common_context(operand))?
                    .flatten()
                    .is_none()
            {
                return Ok(expression);
            }
        }
        let mut positions = ProductionVec::new(*control);
        for (index, item) in list.iter().enumerate() {
            control.check()?;
            if !crate::semantics::references_current_row(
                item,
                self.context.schema.physical_schema(),
            ) {
                positions.push_copy(index)?;
            }
        }
        let mut common = if positions.len() > 1 {
            self.context.common_type(
                std::iter::once(expr.as_ref()).chain(positions.iter().map(|index| &list[*index])),
            )?
        } else {
            None
        };
        if common
            .as_deref()
            .is_some_and(|ty| matches!(ty, ColumnType::Record | ColumnType::Array(_)))
        {
            common = None;
        }
        let positions = positions.finish()?;
        let positions = if let Some(common) = common {
            for index in positions.iter() {
                expression = expression.map_child(index + 1, control, |child| {
                    self.operand_cast(child, &common)
                })?;
            }
            Some(positions)
        } else {
            drop(positions);
            None
        };
        self.build_membership(expression, positions, negated)
    }

    fn build_membership(
        &self,
        expression: OwnedScalarExpr,
        positions: Option<Produced<Vec<usize>>>,
        negated: bool,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let mut children = expression.into_children();
        let mut value = Some(children.next().expect("membership left operand"));
        let mut array = ProductionVec::new(*control);
        let mut individual = ProductionVec::new(*control);
        let mut selected = positions
            .iter()
            .flat_map(|positions| positions.iter())
            .peekable();
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
        drop(positions);
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
        self.coerce_membership_operands(output)
    }

    fn coerce_membership_operands(
        &self,
        mut expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        match &*expression {
            ScalarExpr::Func { .. } => Ok(self
                .coerce_quantified_comparison(OwnedBindingCall::new(expression, control)?)?
                .into_expression()),
            ScalarExpr::Binary { op, lhs, rhs } => {
                let types = self
                    .context
                    .semantic(self.context.binary_operand_types(*op, lhs, rhs))?
                    .flatten();
                if let Some(types) = types {
                    for index in 0..2 {
                        expression = expression.map_child(index, control, |child| {
                            self.operand_cast(child, &types[index])
                        })?;
                    }
                }
                Ok(expression)
            }
            ScalarExpr::And(_) | ScalarExpr::Or(_) => {
                expression.map_children(control, |_, child| self.coerce_membership_operands(child))
            }
            _ => unreachable!("membership analysis produces comparisons"),
        }
    }
}
