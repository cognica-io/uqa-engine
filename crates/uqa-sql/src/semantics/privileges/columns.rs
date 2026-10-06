//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Base columns referenced by an executable query block, including aliases and joined output names.

use super::{
    analyze_query_block, BTreeMap, BTreeSet, CteScope, QueryBlockPlan, SQLError, SourceLineage,
};

pub fn required(
    block: &QueryBlockPlan,
    scope: &CteScope<'_>,
) -> Result<BTreeMap<String, BTreeSet<String>>, SQLError> {
    let mut scope = scope.clone();
    scope.include_authorization_only_columns = false;
    let mut universe = SourceLineage::default();
    let mut required = BTreeSet::new();
    analyze_query_block(
        block,
        block.from.as_ref(),
        &scope,
        &[],
        &mut universe,
        &mut required,
    )?;
    Ok(by_relation(required))
}

fn by_relation(required: BTreeSet<super::BaseColumn>) -> BTreeMap<String, BTreeSet<String>> {
    let mut result: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for column in required {
        result
            .entry(column.table)
            .or_default()
            .insert(column.column);
    }
    result
}

/// Columns read through a mutation's additional sources, using the same aliases as SELECT privilege analysis.
pub fn for_source_expressions(
    source: &crate::plan::SourcePlan,
    expressions: &[&crate::ScalarExpr],
    scope: &CteScope<'_>,
) -> Result<BTreeMap<String, BTreeSet<String>>, SQLError> {
    let mut scope = scope.clone();
    scope.include_authorization_only_columns = false;
    let (_, required) = super::source_expression_dependencies(source, expressions, &scope)?;
    Ok(by_relation(required))
}

/// Columns read from a mutation target, including RETURNING's OLD/NEW aliases and correlated subqueries.
pub fn for_target_expressions(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&crate::ScalarExpr],
    scope: &CteScope<'_>,
) -> Result<BTreeMap<String, BTreeSet<String>>, SQLError> {
    let mut scope = scope.clone();
    scope.include_authorization_only_columns = false;
    let (_, required) = super::table_expression_dependencies(
        table,
        qualifiers,
        expressions,
        &scope.scalar_subqueries,
        &[],
        &scope,
    )?;
    Ok(by_relation(required))
}
