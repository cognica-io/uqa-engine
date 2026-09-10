//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE scope, reachability, and dependency ordering.

use crate::plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan};
use crate::SQLError;
use std::collections::{BTreeMap, BTreeSet};

/// Decode a source reference into a CTE identifier. Qualified relations never resolve to CTEs.
pub fn cte_reference_name(reference: &str) -> Option<String> {
    let (schema, name) = crate::RelationIdentity::parse_reference(reference).ok()?;
    schema.is_none().then_some(name)
}

pub fn reachable_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let targets = plan
        .ctes
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    if targets.is_empty() {
        return BTreeSet::new();
    }

    let mut reachable = plan
        .ctes
        .iter()
        .filter(|cte| cte.body.modifies_data())
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    collect_target_cte_references_from_root(&plan.root, &targets, &BTreeSet::new(), &mut reachable);

    let mut expanded = BTreeSet::new();
    loop {
        let pending = plan
            .ctes
            .iter()
            .enumerate()
            .filter(|(_, cte)| reachable.contains(&cte.name) && !expanded.contains(&cte.name))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for (index, cte) in pending {
            expanded.insert(cte.name.clone());
            let visible_dependencies = if cte.recursive {
                targets.clone()
            } else {
                plan.ctes[..index]
                    .iter()
                    .map(|dependency| dependency.name.clone())
                    .collect::<BTreeSet<_>>()
            };
            collect_target_cte_references_from_body(
                &cte.body,
                &visible_dependencies,
                &BTreeSet::new(),
                &mut reachable,
            );
        }
    }
    reachable
}

pub fn cte_references_own_name(cte: &CtePlan) -> bool {
    let targets = BTreeSet::from([cte.name.clone()]);
    let mut references = BTreeSet::new();
    collect_target_cte_references_from_body(&cte.body, &targets, &BTreeSet::new(), &mut references);
    references.contains(&cte.name)
}

pub fn ordered_plan_ctes(plan: &QueryPlan) -> Result<Vec<&CtePlan>, SQLError> {
    ordered_cte_plans(&plan.ctes)
}

pub fn ordered_cte_plans(ctes: &[CtePlan]) -> Result<Vec<&CtePlan>, SQLError> {
    order_cte_plans(ctes.iter().collect())
}

pub fn order_cte_plans(plans: Vec<&CtePlan>) -> Result<Vec<&CtePlan>, SQLError> {
    if !plans.iter().any(|cte| cte.recursive) {
        return Ok(plans);
    }
    let targets = plans
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    let dependencies = plans
        .iter()
        .map(|cte| {
            let mut references = BTreeSet::new();
            collect_target_cte_references_from_body(
                &cte.body,
                &targets,
                &BTreeSet::new(),
                &mut references,
            );
            references.remove(&cte.name);
            references
        })
        .collect::<Vec<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::with_capacity(plans.len());
    let mut remaining = (0..plans.len()).collect::<BTreeSet<_>>();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .copied()
            .find(|index| dependencies[*index].is_subset(&emitted));
        let Some(index) = ready else {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "mutual recursion between WITH items is not implemented".into(),
            });
        };
        remaining.remove(&index);
        emitted.insert(plans[index].name.clone());
        ordered.push(plans[index]);
    }
    Ok(ordered)
}

/// Return reachable CTEs with exactly one syntactic reference in the owning query tree. Counting references outside their lexical visibility can only make this set more conservative, never cause a multiply referenced CTE to be streamed as a single-consumer input.
pub fn single_reference_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let targets = plan
        .ctes
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    let mut counts = targets
        .iter()
        .map(|name| (name.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    count_plan_cte_references(plan, &targets, &mut counts);
    counts
        .into_iter()
        .filter_map(|(name, count)| (count == 1).then_some(name))
        .collect()
}

fn count_plan_cte_references(
    plan: &QueryPlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    for cte in &plan.ctes {
        count_cte_body_references(&cte.body, targets, counts);
    }
    count_relational_cte_references(&plan.root, targets, counts);
}

fn count_cte_body_references(
    body: &CtePlanBody,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    match body {
        CtePlanBody::Query(query) => count_plan_cte_references(query, targets, counts),
        CtePlanBody::Command(command) => {
            for cte in command.ctes() {
                count_cte_body_references(&cte.body, targets, counts);
            }
            for query in command.query_inputs() {
                count_plan_cte_references(query, targets, counts);
            }
            if let Some(source) = command.source_input() {
                count_source_cte_references(source, targets, counts);
            }
        }
    }
}

fn count_relational_cte_references(
    plan: &RelationalPlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                count_source_cte_references(source, targets, counts);
            }
            for subquery in &block.subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            count_plan_cte_references(left, targets, counts);
            count_plan_cte_references(right, targets, counts);
            for subquery in subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                count_plan_cte_references(subquery, targets, counts);
            }
        }
    }
}

fn count_source_cte_references(
    source: &SourcePlan,
    targets: &BTreeSet<String>,
    counts: &mut BTreeMap<String, usize>,
) {
    match source {
        SourcePlan::Table { name, .. } => {
            if let Some(name) = cte_reference_name(name).filter(|name| targets.contains(name)) {
                *counts.entry(name).or_default() += 1;
            }
        }
        SourcePlan::Join { left, right, .. } => {
            count_source_cte_references(left, targets, counts);
            count_source_cte_references(right, targets, counts);
        }
        SourcePlan::Subquery { body, .. } => {
            count_plan_cte_references(body, targets, counts);
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn collect_target_cte_references_from_root(
    root: &RelationalPlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    match root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_target_cte_references_from_source(source, targets, shadowed, references);
            }
            for subquery in &block.subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_target_cte_references_from_nested_query(left, targets, shadowed, references);
            collect_target_cte_references_from_nested_query(right, targets, shadowed, references);
            for subquery in subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                collect_target_cte_references_from_nested_query(
                    subquery, targets, shadowed, references,
                );
            }
        }
    }
}

fn collect_target_cte_references_from_source(
    source: &SourcePlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    match source {
        SourcePlan::Table { name, .. } => {
            if let Some(name) = cte_reference_name(name)
                .filter(|name| targets.contains(name) && !shadowed.contains(name))
            {
                references.insert(name);
            }
        }
        SourcePlan::Join { left, right, .. } => {
            collect_target_cte_references_from_source(left, targets, shadowed, references);
            collect_target_cte_references_from_source(right, targets, shadowed, references);
        }
        SourcePlan::Subquery { body, .. } => {
            collect_target_cte_references_from_nested_query(body, targets, shadowed, references);
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn collect_target_cte_references_from_command_root(
    command: &CommandPlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    for query in command.query_inputs() {
        collect_target_cte_references_from_nested_query(query, targets, shadowed, references);
    }
    if let Some(source) = command.source_input() {
        collect_target_cte_references_from_source(source, targets, shadowed, references);
    }
}

fn collect_target_cte_references_from_body(
    body: &CtePlanBody,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    let CtePlanBody::Command(command) = body else {
        if let CtePlanBody::Query(query) = body {
            collect_target_cte_references_from_nested_query(query, targets, shadowed, references);
        }
        return;
    };
    let locals = command
        .ctes()
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    let mut reachable = command
        .ctes()
        .iter()
        .filter(|cte| cte.body.modifies_data())
        .map(|cte| cte.name.clone())
        .collect::<BTreeSet<_>>();
    collect_target_cte_references_from_command_root(
        command,
        &locals,
        &BTreeSet::new(),
        &mut reachable,
    );
    let mut expanded = BTreeSet::new();
    loop {
        let pending = command
            .ctes()
            .iter()
            .enumerate()
            .filter(|(_, cte)| reachable.contains(&cte.name) && !expanded.contains(&cte.name))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        for (index, cte) in pending {
            expanded.insert(cte.name.clone());
            let visible = if cte.recursive {
                locals.clone()
            } else {
                command.ctes()[..index]
                    .iter()
                    .map(|cte| cte.name.clone())
                    .collect()
            };
            collect_target_cte_references_from_body(
                &cte.body,
                &visible,
                &BTreeSet::new(),
                &mut reachable,
            );
        }
    }
    let mut root_shadowed = shadowed.clone();
    root_shadowed.extend(locals.iter().cloned());
    collect_target_cte_references_from_command_root(command, targets, &root_shadowed, references);
    let mut preceding = shadowed.clone();
    for cte in command.ctes() {
        if reachable.contains(&cte.name) {
            let definition = if cte.recursive {
                shadowed.union(&locals).cloned().collect()
            } else {
                preceding.clone()
            };
            collect_target_cte_references_from_body(&cte.body, targets, &definition, references);
        }
        preceding.insert(cte.name.clone());
    }
}

fn collect_target_cte_references_from_nested_query(
    plan: &QueryPlan,
    targets: &BTreeSet<String>,
    shadowed: &BTreeSet<String>,
    references: &mut BTreeSet<String>,
) {
    let local_reachable = reachable_plan_cte_names(plan);
    let mut root_shadowed = shadowed.clone();
    root_shadowed.extend(
        plan.ctes
            .iter()
            .map(|cte| cte.name.clone())
            .filter(|name| targets.contains(name)),
    );
    collect_target_cte_references_from_root(&plan.root, targets, &root_shadowed, references);
    let recursive_scope = plan.ctes.iter().any(|cte| cte.recursive).then(|| {
        plan.ctes
            .iter()
            .map(|cte| cte.name.clone())
            .collect::<BTreeSet<_>>()
    });
    let mut preceding = BTreeSet::new();
    for cte in &plan.ctes {
        if local_reachable.contains(&cte.name) {
            let mut definition_shadowed = shadowed.clone();
            if let Some(recursive_scope) = recursive_scope.as_ref() {
                definition_shadowed.extend(
                    recursive_scope
                        .iter()
                        .filter(|name| targets.contains(*name))
                        .cloned(),
                );
            } else {
                definition_shadowed.extend(
                    preceding
                        .iter()
                        .filter(|name| targets.contains(*name))
                        .cloned(),
                );
            }
            collect_target_cte_references_from_body(
                &cte.body,
                targets,
                &definition_shadowed,
                references,
            );
        }
        preceding.insert(cte.name.clone());
    }
}
