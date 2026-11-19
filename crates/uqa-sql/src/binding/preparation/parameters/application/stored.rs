//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Install selections on the native default plan at their original addresses, retaining each written query namespace.

use super::{InputConstants, ProductionControl, SQLError, SQLParam, Work};
use crate::{
    binding::preparation::parameters::{constants::input::Selected, AnalysisIdentity},
    construction,
    plan::{
        ArenaEdit, OwnedScalarExpr, QueryMetadataUpdate, QueryPlan, RetainedExecutablePlan,
        ScalarEdit, ScalarEditIdentity,
    },
    routines::RoutineResolution,
    ScalarExpr,
};

struct Stored<'a> {
    edit: Selected,
    routines: &'a dyn RoutineResolution,
    canonical_types: bool,
    params: &'a [SQLParam],
}

impl ArenaEdit for Stored<'_> {
    fn set_arena(&mut self, width: Option<usize>) {
        self.edit.set_arena(width);
    }
    async fn completed_query(
        &mut self,
        query: &QueryPlan,
        control: &ProductionControl<'_>,
        work: &mut construction::Work<'_>,
    ) -> Result<QueryMetadataUpdate, SQLError> {
        self.edit
            .inputs()
            .8
            .groupings
            .metadata_from(query, self.routines, self.params, control, work)
            .await
    }
}

impl ScalarEditIdentity for Stored<'_> {
    type Identity = AnalysisIdentity<ScalarExpr>;
    const BEFORE_CHILDREN: bool = false;
    fn identity(expression: &ScalarExpr) -> Self::Identity {
        expression.into()
    }
}

impl ScalarEdit<construction::Work<'_>> for Stored<'_> {
    async fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut construction::Work<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let expression = if self.canonical_types {
            crate::binding::composite_inputs::owned::canonical_type_from(
                expression,
                self.routines,
                control,
                work,
            )
            .await?
        } else {
            expression
        };
        self.edit.apply(identity, expression, control, work).await
    }
}

impl InputConstants {
    pub(in crate::binding::preparation) async fn apply_stored_from(
        mut self,
        mut plan: RetainedExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<RetainedExecutablePlan, SQLError> {
        self.apply_metadata_owned_from(&mut plan, control, work)
            .await?;
        if self.is_consumed() {
            control.check()?;
            return Ok(plan);
        }
        let edit = Stored {
            canonical_types: std::mem::take(&mut self.12),
            edit: Selected::new(self),
            routines,
            params,
        };
        let (plan, done) = plan.edit_stored_scalars_from(edit, control, work).await?;
        if !done.edit.into_constants().is_consumed() {
            return Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ));
        }
        control.check()?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests;
