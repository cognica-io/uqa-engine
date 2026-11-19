//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured rule row identities are observed without reallocating or refreshing them.

use super::RuleConditionBinding;
use crate::{
    ast::RuleEvent,
    catalog::work::{inline, retire_entries, ReferenceWork, Work},
    ir::observation::{visit_all, ScalarObserver},
    plan::{visit_scalar_nodes_with_work, ExpressionPlan, OwnedScalarExpr, PlanRef},
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl RuleConditionBinding {
    pub(crate) async fn captured_row_schema_with_work<W: Work + Send>(
        &self,
        row: &RowSchema,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<RowSchema, SQLError> {
        let mut positions = ProductionMap::new(*control);
        for (position, name) in self.columns.iter().enumerate() {
            positions
                .insert_copy_from(
                    name.as_str(),
                    position,
                    &mut ReferenceWork { control, work },
                )
                .await?;
        }
        let mut surviving = uqa_core::memory::ProductionVec::new(*control);
        surviving.reserve(row.len())?;
        for (index, name) in row.columns().iter().enumerate() {
            let name = row.public_name(index).unwrap_or(name);
            let position = positions
                .get_from(name, &mut ReferenceWork { control, work })
                .await?;
            surviving.push_copy(position.copied())?;
        }
        retire_entries(positions.into_iter(), control, work).await?;
        row.with_captured_row_images(
            [("old", self.old_relation), ("new", self.new_relation)],
            &surviving,
            control,
            work,
        )
        .await
    }

    pub(crate) fn current_whole_row(
        &self,
        side: &str,
        table: &str,
        row: &RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        let present = match side {
            "old" => self.old_relation.is_some(),
            "new" => self.new_relation.is_some(),
            _ => false,
        };
        if !present {
            return Ok(None);
        }
        let mut columns = crate::production::Buffer::new(control);
        for (index, name) in row.columns().iter().enumerate() {
            columns.push(
                OwnedScalarExpr::column(
                    control.copy_text(row.public_name(index).unwrap_or(name))?,
                    Some(control.copy_text(side)?),
                    control,
                )?,
                control,
            )?;
        }
        Ok(Some(OwnedScalarExpr::cast(
            OwnedScalarExpr::row(columns.into_iter(), control)?,
            control.copy_text(table)?,
            false,
            control,
        )?))
    }

    #[cfg(test)]
    pub(crate) fn validate_captured(
        &self,
        event: RuleEvent,
        plan: &ExpressionPlan,
        control: &ProductionControl<'_>,
        column: &mut impl FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        inline(self.validate_captured_with_work(event, plan, control, &mut (), column))
    }

    pub(crate) async fn validate_captured_with_work<W: Work + Send>(
        &self,
        event: RuleEvent,
        plan: &ExpressionPlan,
        control: &ProductionControl<'_>,
        work: &mut W,
        column: &mut impl FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let old = matches!(event, RuleEvent::Update | RuleEvent::Delete);
        let new = matches!(event, RuleEvent::Insert | RuleEvent::Update);
        if self.old_relation.is_some() != old || self.new_relation.is_some() != new {
            return Err(invalid("row sides differ from the rule event", control));
        }
        if self.old_relation.is_some() && self.old_relation == self.new_relation {
            return Err(invalid("OLD and NEW share an identity", control));
        }
        if [self.old_relation, self.new_relation]
            .into_iter()
            .flatten()
            .any(|relation| relation.raw() == 0)
        {
            return Err(invalid("zero row identity", control));
        }
        let mut names = ProductionMap::new(*control);
        for name in &self.columns {
            if names
                .insert_copy_from(name.as_str(), (), &mut ReferenceWork { control, work })
                .await?
                .is_some()
            {
                return Err(invalid("duplicate captured column", control));
            }
        }
        let mut observer = CapturedColumns {
            binding: self,
            control,
            column,
        };
        visit_all(&plan.scalar, control, work, &mut observer).await?;
        for query in &plan.subqueries {
            let read =
                visit_scalar_nodes_with_work(PlanRef::Query(query), control, work, &mut observer);
            if W::MAY_SUSPEND {
                crate::construction::retain(*control, read)?.await?;
            } else {
                inline(read)?;
            }
        }
        retire_entries(names.into_iter(), control, work).await?;
        control.check()?;
        Ok(())
    }
}

struct CapturedColumns<'a, 'c, F> {
    binding: &'a RuleConditionBinding,
    control: &'a ProductionControl<'c>,
    column: &'a mut F,
}

impl<F: FnMut(&str) -> Result<(), SQLError>> ScalarObserver for CapturedColumns<'_, '_, F> {
    fn scalar<W: Work + Send>(
        &mut self,
        node: &ScalarExpr,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready(match node {
            ScalarExpr::InternalColumn(reference) => self
                .binding
                .column_name(*reference)
                .ok_or_else(|| invalid("unowned or out-of-range captured column", self.control))
                .and_then(&mut *self.column),
            ScalarExpr::Param(_) => Err(invalid("external parameter", self.control)),
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            } if crate::semantics::function_has_implicit_row_input(binding) => {
                Err(invalid("execution-only implicit row input", self.control))
            }
            _ => Ok(()),
        })
    }
}

fn invalid(detail: &str, control: &ProductionControl<'_>) -> SQLError {
    match control.format(format_args!("invalid captured rule condition: {detail}")) {
        Ok(message) => SQLError::Internal(message.into_parts().0),
        Err(error) => error.into(),
    }
}
