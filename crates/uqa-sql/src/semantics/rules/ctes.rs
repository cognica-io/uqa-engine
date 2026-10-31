//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement's WITH belongs to one rewritten query, never copies of that query.

use super::{RuleCatalog, RuleEvent, SQLError};
use uqa_core::memory::ProductionControl;

pub fn validate_rule_cte_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<String>, SQLError>,
) -> Result<(), SQLError> {
    let control = ProductionControl::uncontrolled();
    super::validate_rule_cte_contract_with_control(
        catalog,
        table,
        event,
        |table, event| {
            original_target(table, event)?
                .map(|name| control.finish(name, None).map_err(Into::into))
                .transpose()
        },
        &control,
    )
}

#[cfg(test)]
mod tests;
