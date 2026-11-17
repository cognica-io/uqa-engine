//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private input destinations keep their original identities until all selected metadata and scalar replacements have transferred.

use super::{AnalysisIdentity, InputConstants, ParameterTypes, QuerySchemas};
use crate::{
    plan::{
        AnalyzedSource, MutableExecutablePlan, QueryMetadataUpdate, RelationalPlan,
        SharedAnalyzedSource, SourceMetadataUpdate, SourcePlan,
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
            plan.apply_projection_aliases(control, &mut |projections| {
                self.projection_aliases(projections, control)
            })?;
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

    pub(super) fn projection_aliases(
        &self,
        projections: &[crate::plan::ProjectionPlan],
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan::ProjectionAliasUpdates, SQLError> {
        let mut aliases = ProductionVec::new(*control);
        for (index, projection) in projections.iter().enumerate() {
            if projection.alias.is_none()
                && self
                    .11
                    .contains_key(&AnalysisIdentity::from(&projection.expr))
            {
                if let crate::ScalarExpr::Column(name)
                | crate::ScalarExpr::QualifiedColumn { column: name, .. } = &projection.expr
                {
                    let name = control.copy_text(name)?;
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
        // Commands have no QueryPlan descriptor carrier; their source schema has already served ordered analysis.
        self.8.sources.remove(&AnalysisIdentity::from(source));
        let mut selected = ProductionVec::new(*control);
        let mut include = |index, binding: &Option<crate::ast::FunctionBinding>| {
            if let Some(binding) = self.7.remove(&AnalysisIdentity::from(binding)) {
                selected.push_produced(
                    control.finish((index, binding), control.empty_reservation())?,
                )?;
            }
            Ok::<_, SQLError>(())
        };
        match source {
            SourcePlan::Function { binding, .. } => include(0, binding)?,
            SourcePlan::FunctionGroup { functions, .. } => {
                for (index, function) in functions.iter().enumerate() {
                    include(index, &function.binding)?;
                }
            }
            _ => {}
        }
        Ok(selected.finish()?)
    }
}

impl QuerySchemas {
    fn take_source_with_control(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<AnalyzedSource>>, SQLError> {
        control.check()?;
        let Some(schema) = self.sources.remove(&AnalysisIdentity::from(source)) else {
            return Ok(None);
        };
        let (children, memory) = match source {
            SourcePlan::Join { left, right, .. } => {
                let Some(left) = self.take_source_with_control(left, control)? else {
                    return Ok(None);
                };
                let Some(right) = self.take_source_with_control(right, control)? else {
                    return Ok(None);
                };
                let header =
                    control.reserve(std::mem::size_of::<(AnalyzedSource, AnalyzedSource)>())?;
                let (left, left_memory) = left.into_parts();
                let (right, right_memory) = right.into_parts();
                let memory = control.combine(control.combine(left_memory, right_memory), header);
                (Some(Box::new((left, right))), memory)
            }
            _ => (None, control.empty_reservation()),
        };
        Ok(Some(
            control.finish(AnalyzedSource { schema, children }, memory)?,
        ))
    }
}
