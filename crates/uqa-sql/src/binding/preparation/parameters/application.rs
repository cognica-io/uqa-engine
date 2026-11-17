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
        MetadataObserver, MutableExecutablePlan, ProjectionAliasUpdates, ProjectionPlan,
        QueryMetadataUpdate, RelationalPlan, SharedAnalyzedSource, SourceMetadataUpdate,
        SourcePlan,
    },
    routines::RoutineResolution,
    SQLError, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl<I: super::LiteralInput> ParameterTypes<'_, I> {
    pub(in crate::binding::preparation) fn apply_owned_query_schemas(
        mut self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut schemas = self.query_schemas.take().expect("query layout observation");
        plan.apply_query_metadata(control, &mut |query| {
            let root = match &query.root {
                RelationalPlan::QueryBlock(block) => self
                    .rewritten_blocks
                    .remove(&AnalysisIdentity::from(block.as_ref())),
                _ => None,
            };
            let selected = root.as_deref().unwrap_or(query);
            let source = match &selected.root {
                RelationalPlan::QueryBlock(block) => block
                    .from
                    .as_ref()
                    .map(|source| schemas.take_source_with_control(source, control))
                    .transpose()?
                    .flatten(),
                _ => None,
            };
            Ok(QueryMetadataUpdate {
                root,
                output: schemas.outputs.remove(&AnalysisIdentity::from(query)),
                source: source
                    .map(|source| SharedAnalyzedSource::from_produced(source, control))
                    .transpose()?,
                ..Default::default()
            })
        })?;
        plan.apply_query_metadata(control, &mut |query| {
            schemas.groupings.metadata(query, routines, params, control)
        })?;
        Ok(())
    }
}

impl InputConstants {
    pub(in crate::binding::preparation) fn apply_owned(
        mut self,
        plan: &mut MutableExecutablePlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        plan.apply_query_metadata(control, &mut |query| {
            let root = match &query.root {
                RelationalPlan::QueryBlock(block) => {
                    self.10.remove(&AnalysisIdentity::from(block.as_ref()))
                }
                _ => None,
            };
            let selected = root.as_deref().unwrap_or(query);
            let source = match &selected.root {
                RelationalPlan::QueryBlock(block) => block
                    .from
                    .as_ref()
                    .map(|source| self.8.take_source_with_control(source, control))
                    .transpose()?
                    .flatten(),
                _ => None,
            };
            Ok(QueryMetadataUpdate {
                root,
                output: self.8.outputs.remove(&AnalysisIdentity::from(query)),
                source: source
                    .map(|source| SharedAnalyzedSource::from_produced(source, control))
                    .transpose()?,
                ..Default::default()
            })
        })?;
        if !self.11.is_empty() {
            inline(self.apply_projection_aliases_owned_from(plan, control, &mut ()))?;
        }
        plan.apply_source_metadata(control, &mut |source| self.source_metadata(source, control))?;
        let mut groupings = std::mem::take(&mut self.8.groupings);
        plan.transform_scalar_arenas_with_metadata(
            control,
            &mut |identity, expression, arena| {
                let identity = AnalysisIdentity::from(identity);
                let expression = match self.2.remove(&identity) {
                    Some(membership) => membership.apply_owned(expression, arena, control)?,
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

    async fn apply_projection_aliases_owned_from<W: Work + Send>(
        &self,
        plan: &mut MutableExecutablePlan,
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

    fn source_metadata(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<SourceMetadataUpdate, SQLError> {
        inline(self.source_metadata_from(source, control, &mut ()))
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
        self.8.sources.remove(&AnalysisIdentity::from(source));
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
