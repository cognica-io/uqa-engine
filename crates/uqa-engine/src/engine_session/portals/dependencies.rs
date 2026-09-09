//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_session_portal_function_relations, Engine, QueryPlan, RelationalPlan, SQLError,
    SessionPortalTableDependencies, SourcePlan,
};
use uqa_sql::registry::FunctionKind;

fn collect_graph_function_dependency(
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    args: &[uqa_execution::ScalarExpr],
    dependencies: &mut SessionPortalTableDependencies,
) {
    if binding.is_some_and(|binding| !binding.builtin) {
        return;
    }
    let name = crate::sql::builtin_function_dispatch_name(name);
    let argument = match uqa_sql::registry::lookup(&name) {
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
    if let Some(uqa_execution::ScalarExpr::Literal(uqa_core::Value::Str(graph))) =
        args.get(argument)
    {
        dependencies.insert_graph(graph.clone());
    } else {
        // Do not evaluate expressions or volatile functions at DECLARE.
        // A parameter, expression, or default graph can select any graph.
        dependencies.graphs = None;
    }
}

pub(super) fn session_portal_table_dependencies(
    engine: &Engine,
    query: &QueryPlan,
) -> Result<SessionPortalTableDependencies, SQLError> {
    let mut dependencies = SessionPortalTableDependencies::empty();
    collect_session_portal_query_dependencies(
        engine,
        query,
        &mut dependencies,
        &mut std::collections::BTreeSet::new(),
        &mut std::collections::BTreeSet::new(),
    )?;
    Ok(dependencies)
}

pub(super) fn collect_session_portal_query_dependencies(
    engine: &Engine,
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
            engine,
            &cte.body,
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
    }
    collect_session_portal_relational_dependencies(
        engine,
        &query.root,
        dependencies,
        visiting_views,
        visiting_routines,
    )?;

    let mut plan = uqa_planner::UnifiedPlan::Query(Box::new(query.clone()));
    let mut routines = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let uqa_execution::ScalarExpr::Func {
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
            engine,
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
    engine: &Engine,
    body: &uqa_planner::CtePlanBody,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match body {
        uqa_planner::CtePlanBody::Query(query) => collect_session_portal_query_dependencies(
            engine,
            query,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        uqa_planner::CtePlanBody::Command(command) => {
            if let Some(target) = command.mutation_target() {
                collect_session_portal_relation_dependencies(
                    engine,
                    target,
                    true,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for cte in command.ctes() {
                collect_session_portal_cte_dependencies(
                    engine,
                    &cte.body,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for query in command.query_inputs() {
                collect_session_portal_query_dependencies(
                    engine,
                    query,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            if let Some(source) = command.source_input() {
                collect_session_portal_source_dependencies(
                    engine,
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

pub(super) fn collect_session_portal_relational_dependencies(
    engine: &Engine,
    plan: &RelationalPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                collect_session_portal_source_dependencies(
                    engine,
                    source,
                    dependencies,
                    visiting_views,
                    visiting_routines,
                )?;
            }
            for subquery in &block.subqueries {
                collect_session_portal_query_dependencies(
                    engine,
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
                engine,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_query_dependencies(
                engine,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            for subquery in subqueries {
                collect_session_portal_query_dependencies(
                    engine,
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
                    engine,
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
    engine: &Engine,
    name: &str,
    include_descendants: bool,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(table) = engine.try_resolve_table_name(name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve cursor dependency relation `{name}`: {error}"
        ))
    })? {
        for table in engine.hierarchy_scan_tables(&table, include_descendants)? {
            dependencies.insert(Engine::resolved_relation_identity(&table).map_err(|error| {
                SQLError::Internal(format!(
                    "resolve cursor dependency identity `{table}`: {error}"
                ))
            })?);
        }
        return Ok(());
    }
    if super::super::canonical_virtual_relation_reference(name).is_some() {
        dependencies.tables = None;
        dependencies.graph_catalog = true;
        return Ok(());
    }
    if let Some(relation) = crate::sql::resolve_age_label_relation_name(engine, name)? {
        let relation = Engine::resolved_relation_identity(&relation)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        dependencies.insert_graph(relation.schema);
        return Ok(());
    }
    let key = name.to_ascii_lowercase();
    if !visiting_views.insert(key.clone()) {
        return Ok(());
    }
    if let Some(view) = engine.view_plan(name)? {
        collect_session_portal_query_dependencies(
            engine,
            &view,
            dependencies,
            visiting_views,
            visiting_routines,
        )?;
    }
    visiting_views.remove(&key);
    Ok(())
}

pub(super) fn collect_session_portal_source_dependencies(
    engine: &Engine,
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
            engine,
            name,
            *include_descendants,
            dependencies,
            visiting_views,
            visiting_routines,
        ),
        SourcePlan::Join { left, right, .. } => {
            collect_session_portal_source_dependencies(
                engine,
                left,
                dependencies,
                visiting_views,
                visiting_routines,
            )?;
            collect_session_portal_source_dependencies(
                engine,
                right,
                dependencies,
                visiting_views,
                visiting_routines,
            )
        }
        SourcePlan::Subquery { body, .. } => collect_session_portal_query_dependencies(
            engine,
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
                engine,
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
                    engine,
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

pub(super) fn collect_session_portal_function_dependencies(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    relations: Option<&uqa_sql::ast::OperatorJoinRelations>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if let Some(relations) = relations {
        for relation in [&relations.left, &relations.right] {
            collect_session_portal_function_relation_dependency(engine, relation, dependencies)?;
        }
    }
    collect_session_portal_routine_dependencies(
        engine,
        name,
        binding,
        dependencies,
        visiting_views,
        visiting_routines,
    )
}

pub(super) fn collect_session_portal_function_relation_dependency(
    engine: &Engine,
    name: &str,
    dependencies: &mut SessionPortalTableDependencies,
) -> Result<(), SQLError> {
    let Some(table) = engine.try_resolve_table_name(name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve cursor table-function relation `{name}`: {error}"
        ))
    })?
    else {
        return Ok(());
    };
    dependencies.insert(Engine::resolved_relation_identity(&table).map_err(|error| {
        SQLError::Internal(format!(
            "resolve cursor table-function relation identity `{table}`: {error}"
        ))
    })?);
    Ok(())
}

pub(super) fn collect_session_portal_routine_dependencies(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut std::collections::BTreeSet<String>,
    visiting_routines: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if binding.is_some_and(|binding| binding.builtin) {
        return Ok(());
    }
    let overloads = match binding {
        Some(binding) => engine.lookup_bound_sql_functions_by_binding(binding),
        None => engine.lookup_visible_sql_functions_for_analysis(name)?,
    };
    let Some(overloads) = overloads else {
        return Ok(());
    };
    for function in overloads {
        if function.def.is_procedure
            || binding.is_some_and(|binding| {
                crate::engine_user_functions::routine_signature_types(&function.def)
                    != binding.argument_types
            })
        {
            continue;
        }
        let signature =
            crate::engine_user_functions::routine_signature_types(&function.def).join(",");
        let key = format!("{}({signature})", function.def.name);
        if !visiting_routines.insert(key.clone()) {
            continue;
        }
        match &function.compiled {
            crate::engine_user_functions::CompiledFunctionBody::SQL(plans) => {
                for plan in plans {
                    match plan {
                        uqa_planner::UnifiedPlan::Query(query) => {
                            collect_session_portal_query_dependencies(
                                engine,
                                query,
                                dependencies,
                                visiting_views,
                                visiting_routines,
                            )?;
                        }
                        uqa_planner::UnifiedPlan::Command(_) => {
                            *dependencies = SessionPortalTableDependencies::all();
                        }
                    }
                    if dependencies.is_all() {
                        break;
                    }
                }
            }
            crate::engine_user_functions::CompiledFunctionBody::PLpgSQL(_) => {
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

pub(super) fn bind_session_portal_query_relations(
    engine: &Engine,
    query: &mut QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &mut query.ctes {
        let mut definition_scope = visible_ctes.clone();
        if cte.recursive {
            definition_scope.insert(cte.name.clone());
        }
        super::super::view_binding::bind_cte_plan_relations(
            &mut cte.body,
            &definition_scope,
            &mut |name| {
                let mut name = name.to_string();
                bind_session_portal_relation_reference(
                    engine,
                    &mut name,
                    &std::collections::BTreeSet::new(),
                )?;
                Ok::<_, SQLError>(name)
            },
        )?;
        visible_ctes.insert(cte.name.clone());
    }
    bind_session_portal_relational_plan(engine, &mut query.root, &visible_ctes)?;
    query.relations_bound = true;
    Ok(())
}

pub(super) fn bind_session_portal_relational_plan(
    engine: &Engine,
    plan: &mut RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_mut() {
                bind_session_portal_source_plan(engine, source, visible_ctes)?;
            }
            for subquery in &mut block.subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            bind_session_portal_query_relations(engine, left, visible_ctes)?;
            bind_session_portal_query_relations(engine, right, visible_ctes)?;
            for subquery in subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                bind_session_portal_query_relations(engine, subquery, visible_ctes)?;
            }
            Ok(())
        }
    }
}

pub(super) fn bind_session_portal_source_plan(
    engine: &Engine,
    source: &mut SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            bind_session_portal_relation_reference(engine, name, visible_ctes)
        }
        SourcePlan::Join { left, right, .. } => {
            bind_session_portal_source_plan(engine, left, visible_ctes)?;
            bind_session_portal_source_plan(engine, right, visible_ctes)
        }
        SourcePlan::Subquery { body, .. } => {
            bind_session_portal_query_relations(engine, body, visible_ctes)
        }
        SourcePlan::Function { relations, .. } => {
            bind_session_portal_function_relations(engine, relations)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                bind_session_portal_function_relations(engine, &mut function.relations)?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

fn bind_session_portal_relation_reference(
    engine: &Engine,
    name: &mut String,
    visible_ctes: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    if crate::RelationIdentity::parse_reference(name)
        .ok()
        .is_some_and(|(schema, name)| schema.is_none() && visible_ctes.contains(&name))
    {
        return Ok(());
    }
    let requested = name.clone();
    if let Some(canonical) = super::super::canonical_virtual_relation_reference(&requested) {
        *name = canonical;
        return Ok(());
    }
    if crate::RelationIdentity::parse_reference(&requested)
        .ok()
        .is_some_and(|(schema, relation)| {
            schema.is_none()
                && crate::sql::active_trigger_transition_relation_names().contains(&relation)
        })
    {
        return Ok(());
    }
    if let Some(canonical) = crate::sql::resolve_age_label_relation_name(engine, &requested)? {
        *name = canonical;
        return Ok(());
    }
    match engine.try_resolve_visible_relation_kind(&requested)? {
        Some((canonical, _)) => *name = canonical,
        None => return Err(SQLError::UnknownTable(requested)),
    }
    Ok(())
}
