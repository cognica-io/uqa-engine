//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary name-returning adapters for the shared borrowed CTE analysis.

use super::cte_order::{
    analysis::CteAnalysis,
    order_cte_plans_with_control,
    references::{self, External, Selection},
};
use crate::plan::{CommandPlan, CtePlan, CtePlanBody, QueryPlan, SourcePlan};
use crate::SQLError;
use std::{cell::RefCell, collections::BTreeSet};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};

/// Decode a source reference into a CTE identifier. Qualified relations never resolve to CTEs.
pub fn cte_reference_name(reference: &str) -> Option<String> {
    let (schema, name) = crate::RelationIdentity::parse_reference(reference).ok()?;
    schema.is_none().then_some(name)
}

pub fn reachable_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    reachable_statement_cte_names(&plan.ctes, primary_query_cte_references(plan))
}

/// The WITH items a command reads, plus every data-modifying item and their dependencies.
pub fn reachable_command_cte_names(command: &CommandPlan) -> BTreeSet<String> {
    reachable_statement_cte_names(
        command.ctes(),
        primary_command_cte_references(
            command.ctes(),
            command.query_inputs(),
            command.source_input(),
        ),
    )
}

/// Resolve the shared dependency closure. Unreferenced SELECT items do not run, even when they contain volatile functions.
pub fn reachable_statement_cte_names(
    ctes: &[CtePlan],
    mut reachable: BTreeSet<String>,
) -> BTreeSet<String> {
    if ctes.is_empty() {
        return BTreeSet::new();
    }
    let mut analysis = CteAnalysis::new(ctes, ProductionControl::uncontrolled())
        .expect("ordinary CTE dependencies");
    analysis
        .set_primary_names(reachable.iter().map(String::as_str))
        .expect("ordinary CTE roots");
    let selected = analysis.reachable().expect("ordinary CTE reachability");
    reachable.extend(
        ctes.iter()
            .enumerate()
            .filter(|(index, _)| selected[*index])
            .map(|(_, cte)| cte.name.clone()),
    );
    reachable
}

/// The WITH items visible to and named by one definition, respecting nested WITH scopes.
pub fn cte_definition_references(ctes: &[CtePlan], index: usize) -> BTreeSet<String> {
    let cte = &ctes[index];
    let ceiling = if cte.recursive { ctes.len() } else { index };
    collect_names(
        ctes[..ceiling].iter().map(|cte| cte.name.as_str()),
        |external| {
            references::body(
                &cte.body,
                external,
                ProductionControl::uncontrolled(),
                Selection::Reachable,
            )
        },
    )
}

/// The WITH items that the primary query names outside the items' own definitions.
pub fn primary_query_cte_references(plan: &QueryPlan) -> BTreeSet<String> {
    primary_query_cte_references_with_control(plan, ProductionControl::uncontrolled())
        .expect("ordinary CTE references")
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

pub fn primary_query_cte_references_with_control<'p>(
    plan: &'p QueryPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<&'p str>>, SQLError> {
    collect_names_with_control(
        plan.ctes.iter().map(|cte| cte.name.as_str()),
        |external| references::query_root(&plan.root, external, control, Selection::Reachable),
        control,
    )
}

/// The WITH items named by a command's own query arenas and source input.
pub fn primary_command_cte_references<'a>(
    ctes: &[CtePlan],
    queries: impl IntoIterator<Item = &'a QueryPlan>,
    source: Option<&SourcePlan>,
) -> BTreeSet<String> {
    primary_command_cte_references_with_control(
        ctes,
        queries,
        source,
        ProductionControl::uncontrolled(),
    )
    .expect("ordinary CTE references")
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

pub fn primary_command_cte_references_with_control<'p, 'q>(
    ctes: &'p [CtePlan],
    queries: impl IntoIterator<Item = &'q QueryPlan>,
    source: Option<&SourcePlan>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<&'p str>>, SQLError> {
    collect_names_with_control(
        ctes.iter().map(|cte| cte.name.as_str()),
        |external| {
            references::command_root(queries, source, external, control, Selection::Reachable)
        },
        control,
    )
}

pub fn reachable_statement_ctes_with_control<'p, 'n>(
    ctes: &'p [CtePlan],
    primary: impl IntoIterator<Item = &'n str>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<&'p CtePlan>>, SQLError> {
    let mut analysis = CteAnalysis::new(ctes, control)?;
    analysis.set_primary_names(primary)?;
    let selected = analysis.reachable()?;
    let mut plans = ProductionVec::new(control);
    for (index, plan) in ctes.iter().enumerate() {
        control.check()?;
        if selected[index] {
            plans.push_copy(plan)?;
        }
    }
    Ok(plans.finish()?)
}

/// Only recursive definitions can reference their own binding.
pub fn cte_references_own_name(cte: &CtePlan) -> bool {
    cte_references_own_name_with_control(cte, ProductionControl::uncontrolled())
        .expect("ordinary CTE self reference")
}

pub fn cte_references_own_name_with_control(
    cte: &CtePlan,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if !cte.recursive {
        return Ok(false);
    }
    let found = std::cell::Cell::new(false);
    references::body(
        &cte.body,
        &|reference| {
            control.check()?;
            if RelationIdentity::borrowed_reference(reference).is_some_and(|reference| {
                reference.schema.is_none() && reference.name == RelationName::decoded(&cte.name)
            }) {
                found.set(true);
            }
            Ok(())
        },
        control,
        Selection::Reachable,
    )?;
    Ok(found.get())
}

/// Already-visible bindings read by a definition, respecting nested WITH scopes.
pub fn cte_external_references(body: &CtePlanBody, visible: &BTreeSet<String>) -> BTreeSet<String> {
    collect_names(visible.iter().map(String::as_str), |external| {
        references::body(
            body,
            external,
            ProductionControl::uncontrolled(),
            Selection::Reachable,
        )
    })
}

fn collect_names<'a>(
    names: impl Iterator<Item = &'a str>,
    visit: impl FnOnce(&External<'_>) -> Result<(), SQLError>,
) -> BTreeSet<String> {
    collect_names_with_control(names, visit, ProductionControl::uncontrolled())
        .expect("ordinary CTE references")
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

fn collect_names_with_control<'p>(
    names: impl Iterator<Item = &'p str>,
    visit: impl FnOnce(&External<'_>) -> Result<(), SQLError>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<&'p str>>, SQLError> {
    let mut retained = ProductionVec::new(control);
    let mut selected = ProductionVec::new(control);
    let mut result = ProductionVec::new(control);
    for name in names {
        retained.push_copy((RelationName::decoded(name), name))?;
        selected.push_copy(false)?;
    }
    if retained.is_empty() {
        return Ok(result.finish()?);
    }
    retained
        .as_mut_slice()
        .sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let selected = RefCell::new(selected);
    visit(&|reference| {
        control.check()?;
        if let Some(reference) = RelationIdentity::borrowed_reference(reference)
            .filter(|reference| reference.schema.is_none())
        {
            if let Ok(index) = retained.binary_search_by(|(name, _)| name.cmp(&reference.name)) {
                selected.borrow_mut().as_mut_slice()[index] = true;
            }
        }
        Ok(())
    })?;
    for ((_, name), selected) in retained.iter().zip(selected.into_inner().iter()) {
        control.check()?;
        if *selected {
            result.push_copy(*name)?;
        }
    }
    Ok(result.finish()?)
}

pub fn ordered_plan_ctes(plan: &QueryPlan) -> Result<Vec<&CtePlan>, SQLError> {
    ordered_cte_plans(&plan.ctes)
}

pub fn ordered_cte_plans(ctes: &[CtePlan]) -> Result<Vec<&CtePlan>, SQLError> {
    Ok(
        order_cte_plans_with_control(ctes, ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary CTE order"),
    )
}

pub fn order_cte_plans(plans: Vec<&CtePlan>) -> Result<Vec<&CtePlan>, SQLError> {
    if !plans.iter().any(|plan| plan.recursive) {
        return Ok(plans);
    }
    Ok(
        order_cte_plans_with_control(plans, ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary CTE order"),
    )
}

/// Definitions with exactly one syntactic reference to their own binding, including references in unread nested definitions.
pub fn single_reference_plan_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let analysis = CteAnalysis::query(plan, ProductionControl::uncontrolled())
        .expect("ordinary CTE reference counts");
    plan.ctes
        .iter()
        .enumerate()
        .filter(|(index, _)| analysis.single_reference(*index))
        .map(|(_, cte)| cte.name.clone())
        .collect()
}
