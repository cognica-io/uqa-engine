//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar positions in a stored mutation retain the original command clause order.

use super::{shape_error, CommandPlan, SQLError, ScalarExpr, SiteInput, Subqueries, Walk};

impl<W: SiteInput> Walk<'_, '_, W>
where
    W::Error: Into<SQLError>,
{
    pub(super) async fn command_scalar_fields(
        &mut self,
        lowered: &CommandPlan,
        bound: &CommandPlan,
        queries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        // A missing position is a statement-shape error before any scalar is paired.
        if self.command_scalar_count(lowered).await? != self.command_scalar_count(bound).await? {
            return Err(shape_error("command expressions"));
        }
        let mut before = lowered.syntax_expression_steps();
        let mut after = bound.syntax_expression_steps();
        while let Some(written) = self.next_command_scalar(&mut before).await? {
            let selected = self
                .next_command_scalar(&mut after)
                .await?
                .expect("checked command width");
            self.scalar(written, selected, queries).await?;
        }
        Ok(())
    }

    async fn command_scalar_count(&mut self, command: &CommandPlan) -> Result<usize, SQLError> {
        let mut count = 0;
        for field in command.syntax_expression_steps() {
            self.checkpoint().await?;
            count += usize::from(field.is_some());
        }
        Ok(count)
    }

    async fn next_command_scalar<'s>(
        &mut self,
        positions: &mut (impl Iterator<Item = Option<&'s ScalarExpr>> + Send),
    ) -> Result<Option<&'s ScalarExpr>, SQLError> {
        for field in positions {
            self.checkpoint().await?;
            if field.is_some() {
                return Ok(field);
            }
        }
        Ok(None)
    }
}
