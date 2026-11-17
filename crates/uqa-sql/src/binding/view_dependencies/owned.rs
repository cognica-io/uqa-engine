//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned relation binding reuses scoped plan traversal and existing source-field replacements.

use crate::{
    ast::OperatorJoinRelations,
    plan::{
        CommandPlan, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        MutableSourcePlan, OwnedScopedPlanVisitor, SourcePlan,
    },
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

type Resolver<'a> =
    dyn FnMut(&str, bool, &ProductionControl<'_>) -> Result<Produced<String>, SQLError> + 'a;

pub(crate) fn bind_query_relation_targets_owned(
    query: MutableQueryPlan,
    inherited: &dyn Fn(&str) -> bool,
    bind_mutations: bool,
    control: &ProductionControl<'_>,
    resolve: &mut Resolver<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    struct Binding<'a, 'r, 'c> {
        resolve: &'a mut Resolver<'r>,
        control: ProductionControl<'c>,
        bind_mutations: bool,
    }
    impl OwnedScopedPlanVisitor for Binding<'_, '_, '_> {
        fn source(
            &mut self,
            source: MutableSourcePlan,
            visible: &dyn Fn(&str) -> bool,
        ) -> Result<(MutableSourcePlan, bool), SQLError> {
            let source = bind_source(source, visible, self.resolve, &self.control)?;
            let descend = !matches!(&*source, SourcePlan::Table { .. });
            Ok((source, descend))
        }
        fn query_complete(
            &mut self,
            query: MutableQueryPlan,
        ) -> Result<MutableQueryPlan, SQLError> {
            query.set_relations_bound(true, &self.control)
        }
        fn command(
            &mut self,
            command: MutableCommandPlan,
            outer: &dyn Fn(&str) -> bool,
        ) -> Result<(MutableCommandPlan, bool), SQLError> {
            if !self.bind_mutations {
                let command = command.map_queries_while(&self.control, &mut |query| {
                    let query = bind_query_relation_targets_owned(
                        query,
                        outer,
                        false,
                        &self.control,
                        self.resolve,
                    )?;
                    Ok((query, false))
                })?;
                return Ok((command, false));
            }
            let Some(target) = command.mutation_target() else {
                return Ok((command, true));
            };
            let (resolve_descendants, descendants) = match &*command {
                CommandPlan::Update(plan) => (plan.include_descendants, plan.include_descendants),
                CommandPlan::Delete(plan) => (plan.include_descendants, plan.include_descendants),
                CommandPlan::Insert(plan) => (false, plan.include_descendants),
                CommandPlan::Merge(plan) => (false, plan.include_descendants),
                _ => unreachable!("mutation target"),
            };
            let target = (self.resolve)(target, resolve_descendants, &self.control)?;
            let command = command
                .retarget_mutation(target, descendants, &self.control)?
                .mark_stored_cte_target_bound(&self.control)?;
            Ok((command, true))
        }
    }
    let mut binding = Binding {
        resolve,
        control: *control,
        bind_mutations,
    };
    let MutableExecutablePlan::Query(query) =
        MutableExecutablePlan::Query(query).visit_scoped_in(control, inherited, &mut binding)?
    else {
        unreachable!("query binding preserves its root");
    };
    Ok(query)
}

fn bind_source(
    mut source: MutableSourcePlan,
    visible: &dyn Fn(&str) -> bool,
    resolve: &mut Resolver<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    match &*source {
        SourcePlan::Table {
            name,
            qualifier,
            include_descendants,
            ..
        } => {
            let qualifier = qualifier
                .is_empty()
                .then(|| local_name(name, control))
                .transpose()?;
            let is_cte = match RelationIdentity::borrowed_reference(name) {
                Some(reference) if reference.schema.is_none() => {
                    let local = control.format(format_args!("{}", reference.name))?;
                    visible(&local)
                }
                _ => false,
            };
            let name = if is_cte {
                None
            } else {
                Some(resolve(name, *include_descendants, control)?)
            };
            source = source.replace_table_reference(name, qualifier, control)?;
        }
        SourcePlan::Join { .. } | SourcePlan::Subquery { .. } => {}
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
                .map(|relations| bound_relations(relations, resolve, control))
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
                    .map(|relations| bound_relations(relations, resolve, control))
                    .transpose()?;
                source = source.replace_function_references(index, output, relations, control)?;
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Values { .. } => {}
    }
    Ok(source)
}

fn bound_relations(
    relations: &OperatorJoinRelations,
    resolve: &mut Resolver<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<OperatorJoinRelations>, SQLError> {
    let left = resolve(&relations.left, false, control)?;
    let right = resolve(&relations.right, false, control)?;
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
