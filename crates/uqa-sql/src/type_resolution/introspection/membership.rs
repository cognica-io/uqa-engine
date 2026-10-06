//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` IN-list analysis: combine multiple non-variable items when their common type has an array, then compare remaining items individually in written order.

use super::{comparison::unknown_input, Binder, ColumnType, SQLError, ScalarExpr};
use crate::type_resolution::membership::{rewrite, MembershipShape};
use uqa_core::memory::ProductionVec;

impl Binder<'_, '_> {
    pub(super) fn coerce_in_list(
        &mut self,
        value: Box<ScalarExpr>,
        mut list: Vec<ScalarExpr>,
        negated: bool,
    ) -> Result<ScalarExpr, SQLError> {
        // Undeclared UQA columns keep their dynamic runtime comparison semantics.
        for expression in std::iter::once(value.as_ref()).chain(list.iter()) {
            if !unknown_input(expression)
                && self
                    .semantic(self.common_context(expression))?
                    .flatten()
                    .is_none()
            {
                return Ok(ScalarExpr::InList {
                    expr: value,
                    list,
                    negated,
                });
            }
        }
        let mut positions = ProductionVec::new(self.control);
        for (index, item) in list.iter().enumerate() {
            self.control.check()?;
            if !crate::semantics::references_current_row(item, self.schema.physical_schema()) {
                positions.push_copy(index)?;
            }
        }
        let mut common = None;
        if positions.len() > 1 {
            common = self.common_type(
                std::iter::once(value.as_ref()).chain(positions.iter().map(|index| &list[*index])),
            )?;
            if common
                .as_deref()
                .is_some_and(|ty| matches!(ty, ColumnType::Record | ColumnType::Array(_)))
            {
                common = None;
            }
        }
        let mut positions = self.retain(positions.finish()?);
        if let Some(common) = common {
            for index in &positions {
                self.operand_cast(&mut list[*index], &common)?;
            }
        } else {
            positions.clear();
        }
        let shape = MembershipShape {
            array_items: positions,
        };
        let rewritten = rewrite(value, list, negated, &shape, &self.control)?;
        let mut expression = self.retain(rewritten);
        self.coerce_membership_operands(&mut expression)?;
        Ok(expression)
    }

    fn coerce_membership_operands(&mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        match expression {
            ScalarExpr::Func { args, .. } => self.coerce_quantified_comparison(args),
            ScalarExpr::Binary { op, lhs, rhs } => {
                let types = self
                    .semantic(self.binary_operand_types(*op, lhs, rhs))?
                    .flatten();
                self.coerce_comparison(types, lhs, rhs)
            }
            ScalarExpr::And(items) | ScalarExpr::Or(items) => {
                for item in items {
                    self.coerce_membership_operands(item)?;
                }
                Ok(())
            }
            _ => unreachable!("membership analysis produces comparison operators"),
        }
    }
}

#[cfg(test)]
mod tests;
