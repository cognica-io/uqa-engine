//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate INSERT rule query shapes before source expressions, CTEs or triggers execute.

use super::{RuleCatalog, RuleEvent, SQLError, Statement};
use crate::plan::{InsertPlan, RelationalPlan};

enum Pending {
    Relation(String, bool, bool),
    Leave,
}

pub fn validate_insert_rule_query_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    insert: &InsertPlan,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<String>, SQLError>,
) -> Result<(), SQLError> {
    let mut pending = vec![Pending::Relation(
        table.into(),
        true,
        conditional_input(insert),
    )];
    let mut active = Vec::new();
    while let Some(next) = pending.pop() {
        crate::expr::check_statement_cancellation()?;
        let Pending::Relation(table, bound, conditional) = next else {
            active.pop();
            continue;
        };
        let table = catalog.resolve_mutation_target(&table, bound)?;
        let relation = catalog.resolve_rule_relation(&table)?;
        let table = relation.qualified_name();
        if active.contains(&table) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "infinite recursion detected in rules for relation \"{}\"",
                    relation.name
                ),
            });
        }
        let rules = catalog.rules_for(&table, RuleEvent::Insert)?;
        active.push(table.clone());
        pending.push(Pending::Leave);
        let survives = !rules
            .iter()
            .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
        if survives {
            if let Some(target) = original_target(&table, RuleEvent::Insert)? {
                pending.push(Pending::Relation(
                    target,
                    true,
                    conditional || rules.iter().any(|rule| rule.definition.instead),
                ));
            }
        }
        for rule in rules.into_iter().rev() {
            let conditional = conditional || rule.definition.condition.is_some();
            for action in rule.definition.actions.into_iter().rev() {
                if conditional && super::action_binding::rule_action_has_set_operation(&action) {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message:
                            "conditional UNION/INTERSECT/EXCEPT statements are not implemented"
                                .into(),
                    });
                }
                if let Statement::Insert(action) = action {
                    pending.push(Pending::Relation(
                        action.table,
                        action.target_relation_bound,
                        conditional || action.select_source.is_some() || action.rows.len() > 1,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn conditional_input(insert: &InsertPlan) -> bool {
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
