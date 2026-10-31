//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate INSERT rule query shapes before source expressions, CTEs or triggers execute.

use super::{RuleCatalog, RuleEvent, SQLError};
use crate::plan::{InsertPlan, RelationalPlan};
use uqa_core::memory::ProductionControl;

pub fn validate_insert_rule_query_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    insert: &InsertPlan,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<String>, SQLError>,
) -> Result<(), SQLError> {
    let control = ProductionControl::uncontrolled();
    super::validate_insert_rule_query_contract_with_control(
        catalog,
        table,
        insert,
        |table, event| {
            original_target(table, event)?
                .map(|name| control.finish(name, None).map_err(Into::into))
                .transpose()
        },
        &control,
    )
}

pub(super) fn conditional_input(insert: &InsertPlan) -> bool {
    let Some(source) = &insert.source else {
        return insert.rows.len() > 1;
    };
    if !insert
        .rule_values_input
        .as_ref()
        .is_some_and(crate::plan::RuleValuesInput::is_inline)
    {
        return true;
    }
    match &source.root {
        RelationalPlan::QueryBlock(block) => {
            !source.ctes.is_empty() || block.from.is_some() || block.r#where.is_some()
        }
        _ => true,
    }
}
