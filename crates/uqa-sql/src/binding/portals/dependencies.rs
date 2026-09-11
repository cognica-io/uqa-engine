//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collect table, graph and routine dependencies without evaluating cursor expressions.

use super::{PortalBindingContext, SessionPortalTableDependencies};
use crate::{
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    registry::FunctionKind,
    SQLError,
};

fn collect_graph_function_dependency(
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    args: &[crate::ScalarExpr],
    dependencies: &mut SessionPortalTableDependencies,
) {
    if binding.is_some_and(|binding| !binding.builtin) {
        return;
    }
    let name = crate::semantics::builtin_function_dispatch_name(name);
    let argument = match crate::registry::lookup(&name) {
        Some(
            FunctionKind::GraphPagerank
            | FunctionKind::GraphHits
            | FunctionKind::GraphBetweenness
            | FunctionKind::GraphTraverse
            | FunctionKind::GraphNeighbors
            | FunctionKind::TraverseMatch
            | FunctionKind::TemporalTraverse
            | FunctionKind::GraphEdges,
        ) => Some(0),
        Some(FunctionKind::RPQ) => Some(2),
        Some(FunctionKind::DeepPredict | FunctionKind::DeepLearn) => {
            // Model programs can select graph inputs dynamically.
            dependencies.graphs = None;
            dependencies.graph_catalog = true;
            return;
        }
        Some(
            FunctionKind::GraphCreate
            | FunctionKind::GraphDrop
            | FunctionKind::GraphExists
            | FunctionKind::GraphLabelCreate
            | FunctionKind::GraphLabelDrop
            | FunctionKind::GraphAlter,
        ) => {
            dependencies.graph_catalog = true;
            return;
        }
        _ if name == "cypher" => Some(0),
        _ if name == "graph_join" => {
            dependencies.graphs = None;
            dependencies.graph_catalog = true;
            return;
        }
        _ => None,
    };
    let Some(argument) = argument else {
        return;
    };
    dependencies.graph_catalog = true;
    if let Some(crate::ScalarExpr::Literal(uqa_core::Value::Str(graph))) = args.get(argument) {
        dependencies.insert_graph(graph.clone());
    } else {
        // Do not evaluate expressions or volatile functions at DECLARE.
        // A parameter, expression, or default graph can select any graph.
        dependencies.graphs = None;
    }
}

pub fn session_portal_table_dependencies(
    inputs: &PortalBindingContext<'_>,
    query: &QueryPlan,
) -> Result<SessionPortalTableDependencies, SQLError> {
    let mut dependencies = SessionPortalTableDependencies::empty();
    collect_session_portal_query_dependencies(
        inputs,
        query,
        &mut dependencies,
        &mut std::collections::BTreeSet::new(),
        &mut std::collections::BTreeSet::new(),
    )?;
    Ok(dependencies)
}

pub fn collect_session_portal_query_dependencies(
    inputs: &PortalBindingContext<'_>,
    query: &QueryPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if dependencies.is_all() {
        return Ok(());
    }
    for cte in &query.ctes {
        collect_session_portal_cte_dependencies(
            inputs,
            &cte.body,
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
    }
    collect_session_portal_relational_dependencies(
        inputs,
        &query.root,
        dependencies,
        visiting_views,
        visiting_routines,
    )?;

    let mut plan = crate::plan::UnifiedPlan::Query(Box::new(query.clone()));
    let mut routines = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let crate::ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        {
            collect_graph_function_dependency(name, binding.as_ref(), args, dependencies);
            routines.push((name.clone(), binding.clone()));
        }
    });
    for (name, binding) in routines {
        collect_session_portal_routine_dependencies(
            inputs,
            &name,
            binding.as_ref(),
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
        if dependencies.is_all() {
            break;
        }
    }
    Ok(())
}

fn collect_session_portal_cte_dependencies(
    inputs: &PortalBindingContext<'_>,
    body: &crate::plan::CtePlanBody,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match body {
        crate::plan::CtePlanBody::Query(query) => collect_session_portal_query_dependencies(
            inputs,
            query,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        crate::plan::CtePlanBody::Command(command) => {
            if let Some(target) = command.mutation_target() {
                collect_session_portal_relation_dependencies(
                    inputs,
                    target,
                    true,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for cte in command.ctes() {
                collect_session_portal_cte_dependencies(
                    inputs,
                    &cte.body,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for query in command.query_inputs() {
                collect_session_portal_query_dependencies(
                    inputs,
                    query,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            if let Some(source) = command.source_input() {
                collect_session_portal_source_dependencies(
                    inputs,
                    source,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            Ok(())
        }
    }
}

pub fn collect_session_portal_relational_dependencies(
    inputs: &PortalBindingContext<'_>,
    plan: &RelationalPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                collect_session_portal_source_dependencies(
                    inputs,
                    source,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for subquery in &block.subqueries {
                collect_session_portal_query_dependencies(
                    inputs,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_session_portal_query_dependencies(
                inputs,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_query_dependencies(
                inputs,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            for subquery in subqueries {
                collect_session_portal_query_dependencies(
                    inputs,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_session_portal_query_dependencies(
                    inputs,
                    subquery,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_session_portal_relation_dependencies(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    include_descendants: bool,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(table) = inputs
        .catalog
        .try_resolve_table_name(name)
        .map_err(|error| {
            SQLError::Internal(format!(
                "resolve cursor dependency relation `{name}`: {error}"
            ))
        })?
    {
        for table in inputs
            .catalog
            .hierarchy_scan_tables(&table, include_descendants)?
        {
            dependencies.insert(
                uqa_core::RelationIdentity::from_legacy_name(&table).map_err(|error| {
                    SQLError::Internal(format!(
                        "resolve cursor dependency identity `{table}`: {error}"
                    ))
                })?,
            );
        }
        return Ok(());
    }
    if crate::binding::view_dependencies::canonical_virtual_relation_reference(name).is_some() {
        dependencies.tables = None;
        dependencies.graph_catalog = true;
        return Ok(());
    }
    if let Some(relation) = inputs.catalog.resolve_age_label_relation_name(name)? {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(&relation).map_err(SQLError::Internal)?;
        dependencies.insert_graph(relation.schema);
        return Ok(());
    }
    let key = name.to_ascii_lowercase();
    if !visiting_views.insert(key.clone()) {
        return Ok(());
    }
    if let Some(view) = inputs.catalog.view_plan(name)? {
        collect_session_portal_query_dependencies(
            inputs,
            &view,
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
    }
    visiting_views.remove(&key);
    Ok(())
}

pub fn collect_session_portal_source_dependencies(
    inputs: &PortalBindingContext<'_>,
    source: &SourcePlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            include_descendants,
            ..
        } => collect_session_portal_relation_dependencies(
            inputs,
            name,
            *include_descendants,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        SourcePlan::Join { left, right, .. } => {
            collect_session_portal_source_dependencies(
                inputs,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_source_dependencies(
                inputs,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )
        }
        SourcePlan::Subquery { body, .. } => collect_session_portal_query_dependencies(
            inputs,
            body,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        SourcePlan::Function {
            name,
            binding,
            relations,
            args,
            ..
        } => {
            collect_graph_function_dependency(name, binding.as_ref(), args, dependencies);
            collect_session_portal_function_dependencies(
                inputs,
                name,
                binding.as_ref(),
                relations.as_ref(),
                dependencies,
                visiting_views,
                visiting_routines,
            )
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                collect_graph_function_dependency(
                    &function.name,
                    function.binding.as_ref(),
                    &function.args,
                    dependencies,
                );
                collect_session_portal_function_dependencies(
                    inputs,
                    &function.name,
                    function.binding.as_ref(),
                    function.relations.as_ref(),
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

pub fn collect_session_portal_function_dependencies(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    relations: Option<&crate::ast::OperatorJoinRelations>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(relations) = relations {
        for relation in [&relations.left, &relations.right] {
            collect_session_portal_function_relation_dependency(inputs, relation, dependencies)?;
        }
    }
    collect_session_portal_routine_dependencies(
        inputs,
        name,
        binding,
        dependencies,
        visiting_views,
        visiting_routines,
    )
}

pub fn collect_session_portal_function_relation_dependency(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    dependencies: &mut SessionPortalTableDependencies,
) -> Result<(), SQLError> {
    let Some(table) = inputs
        .catalog
        .try_resolve_table_name(name)
        .map_err(|error| {
            SQLError::Internal(format!(
                "resolve cursor table-function relation `{name}`: {error}"
            ))
        })?
    else {
        return Ok(());
    };
    dependencies.insert(
        uqa_core::RelationIdentity::from_legacy_name(&table).map_err(|error| {
            SQLError::Internal(format!(
                "resolve cursor table-function relation identity `{table}`: {error}"
            ))
        })?,
    );
    Ok(())
}

pub fn collect_session_portal_routine_dependencies(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if binding.is_some_and(|binding| binding.builtin) {
        return Ok(());
    }
    let overloads = match binding {
        Some(binding) => inputs
            .routines
            .lookup_bound_sql_functions_by_binding(binding),
        None => inputs
            .routines
            .lookup_visible_sql_functions_for_analysis(name)?,
    };
    let Some(overloads) = overloads else {
        return Ok(());
    };
    for function in overloads {
        if function.def.is_procedure
            || binding.is_some_and(|binding| {
                crate::routines::routine_signature_types(&function.def) != binding.argument_types
            })
        {
            continue;
        }
        let signature = crate::routines::routine_signature_types(&function.def).join(",");
        let key = format!("{}({signature})", function.def.name);
        if !visiting_routines.insert(key.clone()) {
            continue;
        }
        match &function.compiled {
            crate::routines::CompiledFunctionBody::SQL(plans) => {
                for plan in plans {
                    match plan {
                        crate::plan::UnifiedPlan::Query(query) => {
                            collect_session_portal_query_dependencies(
                                inputs,
                                query,
                                dependencies,
                                visiting_views,
                                visiting_routines,
                            )?;
                        }
                        crate::plan::UnifiedPlan::Command(_) => {
                            *dependencies = SessionPortalTableDependencies::all();
                        }
                    }
                    if dependencies.is_all() {
                        break;
                    }
                }
            }
            crate::routines::CompiledFunctionBody::PLpgSQL(_) => {
                *dependencies = SessionPortalTableDependencies::all();
            }
        }
        visiting_routines.remove(&key);
        if dependencies.is_all() {
            break;
        }
    }
    Ok(())
}
