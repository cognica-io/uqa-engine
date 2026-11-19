//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private input destinations keep their original identities until all selected metadata and scalar replacements have transferred.

use super::{AnalysisIdentity, InputConstants, ParameterTypes};
use crate::{
    catalog::work::{inline, Work},
    plan::{
        MetadataObserver, MetadataTarget, MutableExecutablePlan, ProjectionAliasUpdates,
        ProjectionPlan, SourceMetadataUpdate, SourcePlan,
    },
    routines::RoutineResolution,
    SQLError, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod layouts;
mod stored;
use layouts::QueryLayouts;

impl<I: super::LiteralInput> ParameterTypes<'_, I> {
    pub(in crate::binding::preparation) fn apply_owned_query_schemas(
        mut self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut schemas = self.query_schemas.take().expect("query layout observation");
        inline(plan.apply_metadata_from(
            &mut QueryLayouts(&mut schemas, &mut self.rewritten_blocks),
            control,
            &mut (),
        ))?;
        plan.apply_query_metadata(control, &mut |query| {
            schemas.groupings.metadata(query, routines, params, control)
        })?;
        Ok(())
    }
}

impl InputConstants {
    pub(in crate::binding::preparation) fn apply_owned(
        self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.apply_with_membership(
            plan,
            routines,
            params,
            control,
            |membership, value, arena| membership.apply_owned(value, arena, control),
        )
    }

    pub(in crate::binding::preparation) fn apply_stored_inline(
        self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.apply_with_membership(
            plan,
            routines,
            params,
            control,
            |membership, value, arena| {
                inline(membership.apply_stored_from(value, arena.queries().len(), control, &mut ()))
            },
        )
    }

    fn apply_with_membership(
        mut self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
        mut install: impl FnMut(
            super::super::membership::MembershipAnalysis,
            crate::plan::OwnedScalarExpr,
            &mut crate::plan::ScalarQueryArena,
        ) -> Result<crate::plan::OwnedScalarExpr, SQLError>,
    ) -> Result<(), SQLError> {
        inline(self.apply_metadata_owned_from(plan, control, &mut ()))?;
        if self.is_consumed() {
            return Ok(control.check()?);
        }
        let canonical_types = std::mem::take(&mut self.12);
        let mut groupings = std::mem::take(&mut self.8.groupings);
        plan.transform_scalar_arenas_with_metadata(
            control,
            &mut |identity, expression, arena| {
                let identity = AnalysisIdentity::from(identity);
                let expression = if canonical_types {
                    crate::binding::composite_inputs::owned::canonical_type(
                        expression, routines, control,
                    )?
                } else {
                    expression
                };
                let expression = match self.2.remove(&identity) {
                    Some(membership) => install(membership, expression, arena)?,
                    None => expression,
                };
                self.apply_node_owned(identity, expression, control)
            },
            &mut |query| groupings.metadata(query, routines, params, control),
        )?;
        if !self.is_consumed() || !groupings.is_empty() {
            return Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ));
        }
        control.check()?;
        Ok(())
    }

    async fn apply_metadata_owned_from<W: Work + Send, P: MetadataTarget>(
        &mut self,
        plan: &mut P,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        plan.apply_metadata_from(&mut QueryLayouts(&mut self.8, &mut self.10), control, work)
            .await?;
        if !self.11.is_empty() {
            self.apply_projection_aliases_owned_from(plan, control, work)
                .await?;
        }
        plan.apply_metadata_from(&mut SourceCalls(self), control, work)
            .await
    }

    async fn apply_projection_aliases_owned_from<W: Work + Send, P: MetadataTarget>(
        &self,
        plan: &mut P,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        plan.apply_metadata_from(&mut ParameterNames(self), control, work)
            .await
    }

    pub(super) fn projection_aliases(
        &self,
        projections: &[crate::plan::ProjectionPlan],
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan::ProjectionAliasUpdates, SQLError> {
        inline(self.projection_aliases_from(projections, control, &mut ()))
    }

    async fn projection_aliases_from<W: Work + Send>(
        &self,
        projections: &[crate::plan::ProjectionPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<crate::plan::ProjectionAliasUpdates, SQLError> {
        let mut aliases = ProductionVec::new(*control);
        for (index, projection) in projections.iter().enumerate() {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if projection.alias.is_none()
                && self
                    .11
                    .contains_key(&AnalysisIdentity::from(&projection.expr))
            {
                if let crate::ScalarExpr::Column(name)
                | crate::ScalarExpr::QualifiedColumn { column: name, .. } = &projection.expr
                {
                    let name = work.text(name, control).await?;
                    aliases.push_produced(
                        control.finish((index, name), control.empty_reservation())?,
                    )?;
                }
            }
        }
        Ok(aliases.finish()?)
    }

    async fn source_metadata_from<W: Work + Send>(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<SourceMetadataUpdate, SQLError> {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        // Commands have no QueryPlan descriptor carrier; their source schema has already served ordered analysis.
        if let Some(schema) = self.8.sources.remove(&AnalysisIdentity::from(source)) {
            schema.retire_from(control, work).await?;
        }
        let mut selected = ProductionVec::new(*control);
        match source {
            SourcePlan::Function { binding, .. } => {
                self.select_source_binding(
                    0,
                    AnalysisIdentity::from(binding),
                    &mut selected,
                    control,
                )?;
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for (index, function) in functions.iter().enumerate() {
                    control.check()?;
                    work.checkpoint().await?;
                    control.check()?;
                    self.select_source_binding(
                        index,
                        AnalysisIdentity::from(&function.binding),
                        &mut selected,
                        control,
                    )?;
                }
            }
            _ => {}
        }
        Ok(selected.finish()?)
    }

    fn select_source_binding(
        &mut self,
        index: usize,
        identity: AnalysisIdentity<Option<crate::ast::FunctionBinding>>,
        selected: &mut ProductionVec<'_, (usize, Produced<crate::ast::FunctionBinding>)>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if let Some(binding) = self.7.remove(&identity) {
            selected
                .push_produced(control.finish((index, binding), control.empty_reservation())?)?;
        }
        Ok(())
    }
}

struct ParameterNames<'a>(&'a InputConstants);

struct SourceCalls<'a>(&'a mut InputConstants);

impl<W: Work + Send> MetadataObserver<W> for SourceCalls<'_> {
    async fn source(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<SourceMetadataUpdate>, SQLError> {
        self.0
            .source_metadata_from(source, control, work)
            .await
            .map(Some)
    }
}

impl<W: Work + Send> MetadataObserver<W> for ParameterNames<'_> {
    fn visit_table_queries(&self) -> bool {
        true
    }

    async fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<ProjectionAliasUpdates>, SQLError> {
        self.0
            .projection_aliases_from(projections, control, work)
            .await
            .map(Some)
    }
}

#[cfg(test)]
mod tests;
