//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply original constant, collation, result and cast selections through their existing owning constructors.

use super::{
    AnalysisIdentity, InputConstants, OwnedScalarExpr, ProductionControl, SQLError, ScalarExpr,
};
use crate::{plan::ScalarInputWork, schema::retention::CatalogRetentionError};

impl InputConstants {
    pub(super) async fn apply_selected_inputs_from<W: ScalarInputWork>(
        &mut self,
        identity: AnalysisIdentity<ScalarExpr>,
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, W::Error>
    where
        W::Error: From<SQLError> + From<CatalogRetentionError>,
    {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        if let Some(collation) = self.6.remove(&identity) {
            if matches!(
                &*expression,
                ScalarExpr::Func {
                    binding: Some(_),
                    ..
                } | ScalarExpr::Binary { .. }
            ) {
                expression = expression.set_call_input_collation(collation, control)?;
            }
        }
        if let Some(constant) = self.0.remove(&identity) {
            expression.retire_from(control, work).await?;
            expression = constant.expression;
        }
        expression = self
            .3
            .apply_owned_from(identity, expression, control, work)
            .await?;
        expression = self
            .4
            .apply_owned_from(identity, expression, control, work)
            .await?;
        if let Some(target) = self.5.remove(&identity) {
            expression = OwnedScalarExpr::cast(
                expression,
                work.type_name(&target, control).await?,
                true,
                control,
            )?;
            target.retire_from(control, work).await?;
        }
        control.check()?;
        Ok(expression)
    }
}

#[cfg(test)]
mod tests;
