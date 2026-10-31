//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IN-list analysis transfers each child into its selected comparison and retires discarded parent storage immediately.

use super::{ColumnType, OwnedBinder, OwnedScalarExpr, SQLError, ScalarExpr};
use crate::type_resolution::{common::unknown_input, OwnedBindingCall};
use uqa_core::memory::{Produced, ProductionVec};

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
        _negated: bool,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let selected = positions.as_deref().map(Vec::as_slice).unwrap_or(&[]);
        let output = crate::type_resolution::membership::rewrite_owned(
            expression,
            selected,
            &self.context.control,
        )?;
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
