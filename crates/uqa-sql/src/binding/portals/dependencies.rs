//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collect table, graph and routine dependencies without evaluating cursor expressions.

use super::{PortalBindingContext, SessionPortalTableDependencies};
use crate::{
    plan::{PlanRef, QueryPlan, RelationalPlan, SourcePlan},
    registry::FunctionKind,
    SQLError,
};

use std::collections::BTreeSet;
use uqa_core::memory::{ProductionControl, ProductionVec};

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
    if let Some(
        crate::ScalarExpr::Literal(uqa_core::Value::Str(graph))
        | crate::ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Str(graph),
            parameter_index: None,
            ..
        },
    ) = args.get(argument)
    {
        // Preparation coerces string constants to their selected SQL type.
        // Those datums still name one graph; parameters and expressions do not.
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
    collect_plan_dependencies(
        inputs,
        PlanRef::Query(query),
        &|_| false,
        &mut dependencies,
        ProductionControl::uncontrolled(),
    )?;
    Ok(dependencies)
}

/// Collect physical inputs of the reachable plan. The caller identifies names provided by an enclosing execution scope. A view or stored routine starts its own SQL namespace.
pub fn collect_plan_dependencies(
    inputs: &PortalBindingContext<'_>,
    plan: PlanRef<'_>,
    visible_cte: &dyn Fn(&str) -> bool,
    dependencies: &mut SessionPortalTableDependencies,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views: &mut BTreeSet::new(),
        visiting_routines: &mut BTreeSet::new(),
        control,
    }
    .plan(plan, visible_cte)
}

pub fn collect_session_portal_query_dependencies(
    inputs: &PortalBindingContext<'_>,
    query: &QueryPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views,
        visiting_routines,
        control: ProductionControl::uncontrolled(),
    }
    .plan(PlanRef::Query(query), &|_| false)
}

pub fn collect_session_portal_relational_dependencies(
    inputs: &PortalBindingContext<'_>,
    plan: &RelationalPlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views,
        visiting_routines,
        control: ProductionControl::uncontrolled(),
    }
    .relational(plan, &|_| false)
}

pub fn collect_session_portal_source_dependencies(
    inputs: &PortalBindingContext<'_>,
    source: &SourcePlan,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views,
        visiting_routines,
        control: ProductionControl::uncontrolled(),
    }
    .source(source, &|_| false)
}

pub fn collect_session_portal_function_dependencies(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    relations: Option<&crate::ast::OperatorJoinRelations>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views,
        visiting_routines,
        control: ProductionControl::uncontrolled(),
    }
    .function(name, binding, relations)
}

pub fn collect_session_portal_routine_dependencies(
    inputs: &PortalBindingContext<'_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    dependencies: &mut SessionPortalTableDependencies,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    Collector {
        inputs,
        dependencies,
        visiting_views,
        visiting_routines,
        control: ProductionControl::uncontrolled(),
    }
    .routine(name, binding)
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

struct Collector<'a, 'c> {
    inputs: &'a PortalBindingContext<'a>,
    dependencies: &'a mut SessionPortalTableDependencies,
    visiting_views: &'a mut BTreeSet<String>,
    visiting_routines: &'a mut BTreeSet<String>,
    control: ProductionControl<'c>,
}

impl Collector<'_, '_> {
    fn plan(&mut self, plan: PlanRef<'_>, outer: &dyn Fn(&str) -> bool) -> Result<(), SQLError> {
        self.control.check()?;
        if self.dependencies.is_all() {
            return Ok(());
        }
        let declarations = match plan {
            PlanRef::Query(query) => &query.ctes[..],
            PlanRef::Command(command) => {
                if let Some(target) = command.mutation_target() {
                    self.relation(target, true, None)?;
                }
                command.ctes()
            }
        };
        if declarations.is_empty() {
            return self.root(plan, outer);
        }
        let analysis = match plan {
            PlanRef::Query(query) => crate::semantics::CteAnalysis::query(query, self.control)?,
            PlanRef::Command(command) => {
                crate::semantics::CteAnalysis::command(command, self.control)?
            }
        };
        let reachable = analysis.reachable()?;
        let recursive_query =
            matches!(plan, PlanRef::Query(_)) && declarations.iter().any(|cte| cte.recursive);
        let visible = |reference: &str, ceiling| {
            analysis.resolves_before(reference, ceiling) || outer(reference)
        };
        for (index, cte) in declarations.iter().enumerate() {
            self.control.check()?;
            if reachable[index] {
                let ceiling = if recursive_query || cte.recursive {
                    declarations.len()
                } else {
                    index
                };
                self.plan((&cte.body).into(), &|name| visible(name, ceiling))?;
                if let Some(cycle) = &cte.cycle {
                    self.scalar_inputs(|visit| {
                        visit(&cycle.mark_value)?;
                        visit(&cycle.mark_default)
                    })?;
                }
            }
        }
        self.root(plan, &|name| visible(name, declarations.len()))
    }

    fn root(&mut self, plan: PlanRef<'_>, visible: &dyn Fn(&str) -> bool) -> Result<(), SQLError> {
        match plan {
            PlanRef::Query(query) => {
                self.relational(&query.root, visible)?;
                self.scalar_inputs(|visit| query.root.try_visit_scalar_inputs(false, visit))
            }
            PlanRef::Command(command) => {
                for query in command.query_inputs() {
                    self.plan(PlanRef::Query(query), visible)?;
                }
                if let Some(source) = command.source_input() {
                    self.source(source, visible)?;
                }
                self.scalar_inputs(|visit| {
                    for expression in command.expressions() {
                        visit(expression)?;
                    }
                    if let Some(source) = command.source_input() {
                        source.try_visit_expressions::<SQLError>(&mut |expression| {
                            visit(expression)?;
                            Ok(true)
                        })?;
                    }
                    Ok(())
                })
            }
        }
    }

    fn relational(
        &mut self,
        plan: &RelationalPlan,
        visible: &dyn Fn(&str) -> bool,
    ) -> Result<(), SQLError> {
        let subqueries = match plan {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    self.source(source, visible)?;
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.plan(PlanRef::Query(left), visible)?;
                self.plan(PlanRef::Query(right), visible)?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            self.plan(PlanRef::Query(query), visible)?;
        }
        Ok(())
    }

    fn scalar_inputs<'q>(
        &mut self,
        roots: impl FnOnce(
            &mut dyn FnMut(&'q crate::ScalarExpr) -> Result<(), SQLError>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let mut routines = ProductionVec::new(self.control);
        roots(&mut |root| {
            root.try_visit_post(&mut |expression| {
                self.control.check()?;
                if let crate::ScalarExpr::Func {
                    name,
                    binding,
                    args,
                    ..
                } = expression
                {
                    collect_graph_function_dependency(
                        name,
                        binding.as_ref(),
                        args,
                        self.dependencies,
                    );
                    if !binding.as_ref().is_some_and(|binding| binding.builtin) {
                        routines.push_copy((name.as_str(), binding.as_ref()))?;
                    }
                }
                Ok(())
            })
        })?;
        for &(name, binding) in routines.iter() {
            self.routine(name, binding)?;
            if self.dependencies.is_all() {
                break;
            }
        }
        Ok(())
    }

    fn source(
        &mut self,
        source: &SourcePlan,
        visible: &dyn Fn(&str) -> bool,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        match source {
            SourcePlan::Table {
                name,
                include_descendants,
                executable_query,
                ..
            } => {
                if visible(name) {
                    return Ok(());
                }
                self.relation(name, *include_descendants, executable_query.as_deref())
            }
            SourcePlan::Join { left, right, .. } => {
                self.source(left, visible)?;
                self.source(right, visible)
            }
            SourcePlan::Subquery { body, .. } => self.plan(PlanRef::Query(body), visible),
            SourcePlan::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                collect_graph_function_dependency(name, binding.as_ref(), args, self.dependencies);
                self.function(name, binding.as_ref(), relations.as_ref())
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    collect_graph_function_dependency(
                        &function.name,
                        function.binding.as_ref(),
                        &function.args,
                        self.dependencies,
                    );
                    self.function(
                        &function.name,
                        function.binding.as_ref(),
                        function.relations.as_ref(),
                    )?;
                }
                Ok(())
            }
            SourcePlan::RetainedRows(_) | SourcePlan::Values { .. } => Ok(()),
        }
    }

    fn function(
        &mut self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        relations: Option<&crate::ast::OperatorJoinRelations>,
    ) -> Result<(), SQLError> {
        if let Some(relations) = relations {
            for relation in [&relations.left, &relations.right] {
                collect_session_portal_function_relation_dependency(
                    self.inputs,
                    relation,
                    self.dependencies,
                )?;
            }
        }
        self.routine(name, binding)
    }

    fn relation(
        &mut self,
        name: &str,
        include_descendants: bool,
        prepared: Option<&QueryPlan>,
    ) -> Result<(), SQLError> {
        if let Some(table) = self
            .inputs
            .catalog
            .try_resolve_table_name(name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "resolve cursor dependency relation `{name}`: {error}"
                ))
            })?
        {
            for table in self
                .inputs
                .catalog
                .hierarchy_scan_tables(&table, include_descendants)?
            {
                self.dependencies.insert(
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
            self.dependencies.tables = None;
            self.dependencies.graph_catalog = true;
            return Ok(());
        }
        if let Some(relation) = self.inputs.catalog.resolve_age_label_relation_name(name)? {
            let relation = uqa_core::RelationIdentity::from_legacy_name(&relation)
                .map_err(SQLError::Internal)?;
            self.dependencies.insert_graph(relation.schema);
            return Ok(());
        }
        let key = name.to_ascii_lowercase();
        if !self.visiting_views.insert(key.clone()) {
            return Ok(());
        }
        if let Some(view) = prepared {
            self.plan(PlanRef::Query(view), &|_| false)?;
        } else if let Some(view) = self.inputs.catalog.view_plan(name)? {
            self.plan(PlanRef::Query(&view), &|_| false)?;
        }
        self.visiting_views.remove(&key);
        Ok(())
    }

    fn routine(
        &mut self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
    ) -> Result<(), SQLError> {
        if binding.is_some_and(|binding| binding.builtin) {
            return Ok(());
        }
        let overloads = match binding {
            Some(binding) => self
                .inputs
                .routines
                .lookup_bound_sql_functions_by_binding(binding),
            None => self
                .inputs
                .routines
                .lookup_visible_sql_functions_for_analysis(name)?,
        };
        let Some(overloads) = overloads else {
            return Ok(());
        };
        for function in overloads {
            if function.def.is_procedure
                || binding.is_some_and(|binding| {
                    crate::routines::routine_signature_types(&function.def)
                        != binding.argument_types
                })
            {
                continue;
            }
            let signature = crate::routines::routine_signature_types(&function.def).join(",");
            let key = format!("{}({signature})", function.def.name);
            if !self.visiting_routines.insert(key.clone()) {
                continue;
            }
            match crate::routines::analyzable_routine_body(self.inputs.routines, &function)?
                .as_deref()
                .map(|body| &**body)
            {
                Some(crate::routines::CompiledFunctionBody::SQL(plans)) => {
                    for plan in plans {
                        match plan {
                            crate::plan::UnifiedPlan::Query(query) => {
                                self.plan(PlanRef::Query(query), &|_| false)?;
                            }
                            crate::plan::UnifiedPlan::Command(_) => {
                                *self.dependencies = SessionPortalTableDependencies::all();
                            }
                        }
                        if self.dependencies.is_all() {
                            break;
                        }
                    }
                }
                Some(crate::routines::CompiledFunctionBody::PLpgSQL(_)) => {
                    *self.dependencies = SessionPortalTableDependencies::all();
                }
                // Procedural fragments may be valid only under the settings in
                // effect when execution reaches them. Unknown dependencies retain
                // all tables without parsing or caching those fragments here.
                None if function.def.language == "plpgsql" => {
                    *self.dependencies = SessionPortalTableDependencies::all();
                }
                None => {}
            }
            self.visiting_routines.remove(&key);
            if self.dependencies.is_all() {
                break;
            }
        }
        Ok(())
    }
}
