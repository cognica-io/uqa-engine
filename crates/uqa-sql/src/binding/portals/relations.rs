//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind cursor relation names while preserving CTE and transition-relation visibility.

use super::PortalBindingContext;
use crate::{
    ast::OperatorJoinRelations,
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};

pub fn bind_session_portal_query_relations(
    inputs: &PortalBindingContext<'_>,
    query: &mut QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &mut query.ctes {
        let mut definition_scope = visible_ctes.clone();
        if cte.recursive {
            definition_scope.insert(cte.name.clone());
        }
        crate::binding::view_dependencies::bind_cte_plan_relations(
            &mut cte.body,
            &definition_scope,
            &mut |name| {
                let mut name = name.to_string();
                bind_session_portal_relation_reference(
                    inputs,
                    &mut name,
                    &std::collections::BTreeSet::new(),
                )?;
                Ok::<_, SQLError>(name)
            },
        )?;
        visible_ctes.insert(cte.name.clone());
    }
    bind_session_portal_relational_plan(inputs, &mut query.root, &visible_ctes)?;
    query.relations_bound = true;
    Ok(())
}

pub fn bind_session_portal_relational_plan(
    inputs: &PortalBindingContext<'_>,
    plan: &mut RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_mut() {
                bind_session_portal_source_plan(inputs, source, visible_ctes)?;
            }
            for subquery in &mut block.subqueries {
                bind_session_portal_query_relations(inputs, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            bind_session_portal_query_relations(inputs, left, visible_ctes)?;
            bind_session_portal_query_relations(inputs, right, visible_ctes)?;
            for subquery in subqueries {
                bind_session_portal_query_relations(inputs, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                bind_session_portal_query_relations(inputs, subquery, visible_ctes)?;
            }
            Ok(())
        }
    }
}

pub fn bind_session_portal_source_plan(
    inputs: &PortalBindingContext<'_>,
    source: &mut SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            bind_session_portal_relation_reference(inputs, name, visible_ctes)
        }
        SourcePlan::Join { left, right, .. } => {
            bind_session_portal_source_plan(inputs, left, visible_ctes)?;
            bind_session_portal_source_plan(inputs, right, visible_ctes)
        }
        SourcePlan::Subquery { body, .. } => {
            bind_session_portal_query_relations(inputs, body, visible_ctes)
        }
        SourcePlan::Function { relations, .. } => {
            bind_session_portal_function_relations(inputs, relations)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                bind_session_portal_function_relations(inputs, &mut function.relations)?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

fn bind_session_portal_relation_reference(
    inputs: &PortalBindingContext<'_>,
    name: &mut String,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if uqa_core::RelationIdentity::parse_reference(name)
        .ok()
        .is_some_and(|(schema, name)| schema.is_none() && visible_ctes.contains(&name))
    {
        return Ok(());
    }
    let requested = name.clone();
    if let Some(canonical) =
        crate::binding::view_dependencies::canonical_virtual_relation_reference(&requested)
    {
        *name = canonical;
        return Ok(());
    }
    if uqa_core::RelationIdentity::parse_reference(&requested)
        .ok()
        .is_some_and(|(schema, relation)| {
            schema.is_none()
                && inputs
                    .transitions
                    .active_transition_relation_names()
                    .contains(&relation)
        })
    {
        return Ok(());
    }
    if let Some(canonical) = inputs.catalog.resolve_age_label_relation_name(&requested)? {
        *name = canonical;
        return Ok(());
    }
    match inputs
        .catalog
        .try_resolve_visible_relation_kind(&requested)?
    {
        Some((canonical, _)) => *name = canonical,
        None => return Err(SQLError::UnknownTable(requested)),
    }
    Ok(())
}

pub fn bind_session_portal_function_relations(
    inputs: &PortalBindingContext<'_>,
    relations: &mut Option<OperatorJoinRelations>,
) -> Result<(), SQLError> {
    let Some(relations) = relations else {
        return Ok(());
    };
    for relation in [&mut relations.left, &mut relations.right] {
        let requested = relation.clone();
        match inputs
            .catalog
            .try_resolve_visible_relation_kind(&requested)?
        {
            Some((canonical, "table")) => *relation = canonical,
            Some((canonical, kind)) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "cursor table-function relation \"{canonical}\" is a {kind}, not a table"
                    ),
                });
            }
            None => return Err(SQLError::UnknownTable(requested)),
        }
    }
    Ok(())
}
