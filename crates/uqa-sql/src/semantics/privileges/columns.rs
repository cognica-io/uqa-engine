//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Base columns referenced by an executable query block, including aliases and joined output names.

use super::{
    analyze_query_block,
    ownership::{self, Required},
    BTreeSet, CteScope, QueryBlockPlan, SQLError, SourceLineage,
};

use uqa_core::memory::{OwnedMap, OwnedSet, Produced};

pub type ColumnDependencies = OwnedMap<Produced<String>, OwnedSet<Produced<String>>>;

pub fn required(
    block: &QueryBlockPlan,
    scope: &CteScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let mut scope = scope.reborrow()?;
    scope.include_authorization_only_columns = false;
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    analyze_query_block(
        block,
        block.from.as_ref(),
        &scope,
        &[],
        &mut universe,
        &mut required,
    )?;
    by_relation(required, &scope)
}

fn by_relation(
    required: Required,
    scope: &CteScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let control = scope.control();
    let mut result = ColumnDependencies::new();
    for column in required {
        let mut names = OwnedSet::new();
        ownership::insert_set(&mut names, column.column, &control)?;
        merge_entry(&mut result, column.table, names, &control)?;
    }
    Ok(result)
}

pub fn merge(
    into: &mut ColumnDependencies,
    other: ColumnDependencies,
    scope: &CteScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<(), SQLError> {
    for (name, columns) in other {
        merge_entry(into, name, columns, &scope.control())?;
    }
    Ok(())
}

fn merge_entry(
    into: &mut ColumnDependencies,
    name: Produced<String>,
    columns: OwnedSet<Produced<String>>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(existing) = into.get_mut(name.as_str()) {
        existing.append(columns);
    } else {
        match control.budget() {
            Some(budget) => {
                into.insert_budgeted(name, columns, budget)?;
            }
            None => {
                into.insert(name, columns);
            }
        }
    }
    Ok(())
}

/// Columns read through a mutation's additional sources, using the same aliases as SELECT privilege analysis.
pub fn for_source_expressions(
    source: &crate::plan::SourcePlan,
    expressions: &[&crate::ScalarExpr],
    scope: &CteScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let mut scope = scope.reborrow()?;
    scope.include_authorization_only_columns = false;
    let (_, required) = super::source_expression_dependencies(source, expressions, &scope)?;
    by_relation(required, &scope)
}

/// Columns read from a mutation target, including RETURNING's OLD/NEW aliases and correlated subqueries.
pub fn for_target_expressions(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&crate::ScalarExpr],
    scope: &CteScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let mut scope = scope.reborrow()?;
    scope.include_authorization_only_columns = false;
    let (_, required) = super::table_expression_dependencies(
        table,
        qualifiers,
        expressions,
        scope.scalar_subqueries,
        &[],
        &scope,
    )?;
    by_relation(required, &scope)
}
