//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline relation capture preserves lexical CTE shielding and admits selected source names before replacing their original owners.

use super::RoutineInliningContext;
use crate::{
    ast::OperatorJoinRelations,
    binding::production::BindingNameSet,
    catalog::resolution::RelationLookupMode,
    plan::{
        MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan, QueryChildSlot, RelationalPlan,
        SourceChildSlot, SourcePlan,
    },
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

impl RoutineInliningContext<'_> {
    pub(super) fn bind_inline_relations_owned(
        &self,
        query: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        let namespace = self.catalog.stored_query_namespace_with_control(control)?;
        let mode = if query.relations_bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        };
        bind_query(self, query, &namespace.transition_relations, mode, control)
    }
}

fn bind_query(
    context: &RoutineInliningContext<'_>,
    mut query: MutableQueryPlan,
    inherited: &BindingNameSet,
    mode: RelationLookupMode,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let mut visible = inherited.copy_with_control(control)?;
    let recursive = query.ctes.iter().any(|cte| cte.recursive);
    let recursive_names = if recursive {
        let mut names = inherited.copy_with_control(control)?;
        for cte in &query.ctes {
            names.insert_with_control(&cte.name, control)?;
        }
        Some(names)
    } else {
        None
    };
    for index in 0..query.ctes.len() {
        query = query.map_cte(index, control, |body| match body {
            MutableExecutablePlan::Query(body) => bind_query(
                context,
                body,
                recursive_names.as_ref().unwrap_or(&visible),
                mode,
                control,
            )
            .map(MutableExecutablePlan::Query),
            MutableExecutablePlan::Command(command) => {
                // SQL table expansion has already declined data-modifying CTEs; a retained command can still contain a query utility body.
                command
                    .map_queries_while(control, &mut |body| {
                        Ok((
                            bind_query(
                                context,
                                body,
                                recursive_names.as_ref().unwrap_or(&visible),
                                mode,
                                control,
                            )?,
                            false,
                        ))
                    })
                    .map(MutableExecutablePlan::Command)
            }
        })?;
        visible.insert_with_control(&query.ctes[index].name, control)?;
    }
    match &query.root {
        RelationalPlan::QueryBlock(_) => {
            query = query.map_source(control, |source| {
                bind_source(context, source, &visible, mode, control)
            })?;
        }
        RelationalPlan::SetOp { .. } => {
            query = query.map_query_child(QueryChildSlot::Left, control, |query| {
                bind_query(context, query, &visible, mode, control)
            })?;
            query = query.map_query_child(QueryChildSlot::Right, control, |query| {
                bind_query(context, query, &visible, mode, control)
            })?;
        }
        RelationalPlan::Values { .. } => {}
    }
    let count = match &query.root {
        RelationalPlan::QueryBlock(block) => block.subqueries.len(),
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries.len()
        }
    };
    for index in 0..count {
        query = query.map_query_child(QueryChildSlot::Subquery(index), control, |query| {
            bind_query(context, query, &visible, mode, control)
        })?;
    }
    query.set_relations_bound(true, control)
}

fn bind_source(
    context: &RoutineInliningContext<'_>,
    mut source: MutableSourcePlan,
    visible: &BindingNameSet,
    mode: RelationLookupMode,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    match &*source {
        SourcePlan::Table {
            name, qualifier, ..
        } => {
            let qualifier = qualifier
                .is_empty()
                .then(|| local_name(name, control))
                .transpose()?;
            let is_cte = match RelationIdentity::borrowed_reference(name) {
                Some(reference) if reference.schema.is_none() => {
                    let local = control.format(format_args!("{}", reference.name))?;
                    visible.contains(&local)
                }
                _ => false,
            };
            let name = if is_cte {
                None
            } else {
                Some(canonical(context, name, mode, control)?)
            };
            source = source.replace_table_reference(name, qualifier, control)?;
        }
        SourcePlan::Join { .. } => {
            source = source.map_child(SourceChildSlot::Left, control, |source| {
                bind_source(context, source, visible, mode, control)
            })?;
            source = source.map_child(SourceChildSlot::Right, control, |source| {
                bind_source(context, source, visible, mode, control)
            })?;
        }
        SourcePlan::Subquery { .. } => {
            source = source.map_query(control, |query| {
                bind_query(context, query, visible, mode, control)
            })?;
        }
        SourcePlan::Function {
            name,
            output_name,
            relations,
            ..
        } => {
            let output = output_name
                .is_empty()
                .then(|| local_name(name, control))
                .transpose()?;
            let relations = relations
                .as_ref()
                .map(|relations| bound_relations(context, relations, mode, control))
                .transpose()?;
            source = source.replace_function_references(0, output, relations, control)?;
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let count = functions.len();
            for index in 0..count {
                let SourcePlan::FunctionGroup { functions, .. } = &*source else {
                    unreachable!();
                };
                let function = &functions[index];
                let output = function
                    .output_name
                    .is_empty()
                    .then(|| local_name(&function.name, control))
                    .transpose()?;
                let relations = function
                    .relations
                    .as_ref()
                    .map(|relations| bound_relations(context, relations, mode, control))
                    .transpose()?;
                source = source.replace_function_references(index, output, relations, control)?;
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Values { .. } => {}
    }
    Ok(source)
}

fn canonical(
    context: &RoutineInliningContext<'_>,
    name: &str,
    mode: RelationLookupMode,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    crate::binding::stored_relations::canonical_catalog_relation_with_control(
        context.relations,
        name,
        mode,
        false,
        "function",
        control,
    )
    .map(|(name, _)| name)
}

fn bound_relations(
    context: &RoutineInliningContext<'_>,
    relations: &OperatorJoinRelations,
    mode: RelationLookupMode,
    control: &ProductionControl<'_>,
) -> Result<Produced<OperatorJoinRelations>, SQLError> {
    let left = canonical(context, &relations.left, mode, control)?;
    let right = canonical(context, &relations.right, mode, control)?;
    let (left, left_memory) = left.into_parts();
    let (right, right_memory) = right.into_parts();
    Ok(control.finish(
        OperatorJoinRelations { left, right },
        control.combine(left_memory, right_memory),
    )?)
}

fn local_name(name: &str, control: &ProductionControl<'_>) -> Result<Produced<String>, SQLError> {
    Ok(match RelationIdentity::borrowed_reference(name) {
        Some(reference) => control.format(format_args!("{}", reference.name))?,
        None => control.copy_text(name)?,
    })
}
