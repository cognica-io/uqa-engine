//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enumerate the CTE names owned by a query and its sources.

use crate::binding::production::BindingNameSet;
use crate::plan::{CtePlanBody, QueryPlan, RelationalPlan, SourcePlan};
use crate::SQLError;
use std::collections::BTreeSet;
use uqa_core::memory::ProductionControl;

pub fn query_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_query_cte_names(plan, &mut names);
    names
}

pub fn query_cte_names_with_control(
    plan: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<BindingNameSet, SQLError> {
    let mut names = BindingNameSet::new();
    visit_query(plan, control, &mut |name| {
        names.insert_with_control(name, control).map(|_| ())
    })?;
    Ok(names)
}

pub fn collect_query_cte_names(plan: &QueryPlan, names: &mut BTreeSet<String>) {
    visit_query(plan, &ProductionControl::uncontrolled(), &mut |name| {
        names.insert(name.to_owned());
        Ok(())
    })
    .expect("ordinary CTE name traversal");
}

pub fn collect_source_query_cte_names(source: &SourcePlan, names: &mut BTreeSet<String>) {
    visit_source(source, &ProductionControl::uncontrolled(), &mut |name| {
        names.insert(name.to_owned());
        Ok(())
    })
    .expect("ordinary source CTE name traversal");
}

fn visit_query(
    plan: &QueryPlan,
    control: &ProductionControl<'_>,
    name: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    for cte in &plan.ctes {
        control.check()?;
        name(&cte.name)?;
        visit_body(&cte.body, control, name)?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                visit_source(source, control, name)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            visit_query(left, control, name)?;
            visit_query(right, control, name)?;
        }
        RelationalPlan::Values { .. } => {}
    }
    Ok(())
}

fn visit_body(
    body: &CtePlanBody,
    control: &ProductionControl<'_>,
    name: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    match body {
        CtePlanBody::Query(query) => visit_query(query, control, name)?,
        CtePlanBody::Command(command) => {
            for cte in command.ctes() {
                control.check()?;
                name(&cte.name)?;
                visit_body(&cte.body, control, name)?;
            }
            for query in command.query_inputs() {
                visit_query(query, control, name)?;
            }
            if let Some(source) = command.source_input() {
                visit_source(source, control, name)?;
            }
        }
    }
    Ok(())
}

fn visit_source(
    source: &SourcePlan,
    control: &ProductionControl<'_>,
    name: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Join { left, right, .. } => {
            visit_source(left, control, name)?;
            visit_source(right, control, name)?;
        }
        SourcePlan::Subquery { body, .. } => visit_query(body, control, name)?,
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn cte_name_capture_admits_nested_names_and_releases_partial_destinations() {
        let crate::plan::UnifiedPlan::Query(mut query) = crate::plan::UnifiedPlan::lower(
            crate::compile("WITH outer_name AS (WITH nested AS (SELECT 1) SELECT * FROM nested), changed AS (WITH input AS (SELECT 2) INSERT INTO target SELECT * FROM input RETURNING *) SELECT * FROM (WITH child AS (SELECT 3) SELECT * FROM child) q UNION ALL SELECT * FROM outer_name")
                .unwrap().remove(0),
        ) else { panic!("query") };
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let names = query_cte_names_with_control(&query, &control).unwrap();
        assert_eq!(
            names.iter().cloned().collect::<BTreeSet<_>>(),
            query_cte_names(&query)
        );
        for name in ["outer_name", "nested", "changed", "input", "child"] {
            assert!(names.contains(name), "{name}");
        }
        assert!(budget.used() > 0);
        drop(names);
        assert_eq!(budget.used(), 0);
        query.ctes.last_mut().unwrap().name = "x".repeat(4 << 20);
        let before = allocation_counter::measure(|| {
            drop(query_cte_names(&query));
        });
        assert!(before.bytes_total >= 4 << 20, "{before:?}");
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                query_cte_names_with_control(&query, &control)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(allocation.bytes_total < 64 << 10, "{allocation:?}");
        assert_eq!(budget.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                query_cte_names_with_control(&query, &control)
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(budget.used(), 0);
            token.reset();
        }
    }
}
