//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private input destinations keep their original identities until all selected metadata and scalar replacements have transferred.

use super::{InputConstants, NonNull, ParameterTypes, QuerySchemas};
use crate::{
    plan::{
        AnalyzedSource, MutableExecutablePlan, QueryMetadataUpdate, RelationalPlan,
        SharedAnalyzedSource, SourceMetadataUpdate, SourcePlan,
    },
    routines::RoutineResolution,
    SQLError, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

impl ParameterTypes<'_> {
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
                RelationalPlan::QueryBlock(block) => {
                    self.rewritten_blocks.remove(&NonNull::from(block.as_ref()))
                }
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
                output: schemas.outputs.remove(&NonNull::from(query)),
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
                RelationalPlan::QueryBlock(block) => self.10.remove(&NonNull::from(block.as_ref())),
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
            let mut aliases = ProductionVec::new(*control);
            if let RelationalPlan::QueryBlock(block) = &selected.root {
                for (index, projection) in block.projections.iter().enumerate() {
                    if projection.alias.is_none()
                        && self.11.contains_key(&NonNull::from(&projection.expr))
                    {
                        if let crate::ScalarExpr::Column(name)
                        | crate::ScalarExpr::QualifiedColumn { column: name, .. } =
                            &projection.expr
                        {
                            let name = control.copy_text(name)?;
                            aliases.push_produced(
                                control.finish((index, name), control.empty_reservation())?,
                            )?;
                        }
                    }
                }
            }
            Ok(QueryMetadataUpdate {
                projection_aliases: if aliases.is_empty() {
                    None
                } else {
                    Some(aliases.finish()?)
                },
                root,
                output: self.8.outputs.remove(&NonNull::from(query)),
                source: source
                    .map(|source| SharedAnalyzedSource::from_produced(source, control))
                    .transpose()?,
                ..Default::default()
            })
        })?;
        plan.apply_source_metadata(control, &mut |source| self.source_metadata(source, control))?;
        plan.transform_scalar_nodes_in_place(control, &mut |identity, expression| {
            self.apply_node_owned(identity, expression, control)
        })?;
        plan.apply_query_metadata(control, &mut |query| {
            self.8.groupings.metadata(query, routines, params, control)
        })?;
        if !self.2.is_empty() {
            plan.transform_scalar_arenas_in_place(control, &mut |identity, expression, arena| {
                match self.2.remove(&identity) {
                    Some(membership) => membership.apply_owned(expression, arena, control),
                    None => Ok(expression),
                }
            })?;
        }
        if !self.is_consumed() {
            return Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ));
        }
        control.check()?;
        Ok(())
    }

    fn source_metadata(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<SourceMetadataUpdate, SQLError> {
        // Commands have no QueryPlan descriptor carrier; their source schema has already served ordered analysis.
        self.8.sources.remove(&NonNull::from(source));
        let mut selected = ProductionVec::new(*control);
        let mut include = |index, binding: &Option<crate::ast::FunctionBinding>| {
            if let Some(binding) = self.7.remove(&NonNull::from(binding)) {
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
        let Some(schema) = self.sources.remove(&NonNull::from(source)) else {
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
