//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement's WITH belongs to one rewritten query, never copies of that query.

use super::{RuleCatalog, RuleEvent, SQLError, Statement};

enum Pending {
    Relation(String, bool, RuleEvent),
    Query,
    Leave,
}

pub fn validate_rule_cte_contract(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<String>, SQLError>,
) -> Result<(), SQLError> {
    let mut pending = vec![Pending::Relation(table.into(), true, event)];
    let mut active: Vec<(String, RuleEvent)> = Vec::new();
    let mut count = 0;
    while let Some(next) = pending.pop() {
        crate::expr::check_statement_cancellation()?;
        match next {
            Pending::Leave => {
                active.pop();
            }
            Pending::Query => count += 1,
            Pending::Relation(table, bound, event) => {
                let table = catalog.resolve_mutation_target(&table, bound)?;
                let relation = catalog.resolve_rule_relation(&table)?;
                let table = relation.qualified_name();
                if active
                    .iter()
                    .any(|entry| entry.0 == table && entry.1 == event)
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42P17".into(),
                        message: format!(
                            "infinite recursion detected in rules for relation \"{}\"",
                            relation.name
                        ),
                    });
                }
                let rules = catalog.rules_for(&table, event)?;
                let survives = !rules
                    .iter()
                    .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
                let original = if survives {
                    Some(
                        original_target(&table, event)?.map_or(Pending::Query, |target| {
                            Pending::Relation(target, true, event)
                        }),
                    )
                } else {
                    None
                };
                active.push((table, event));
                pending.push(Pending::Leave);
                if let Some(original) = original {
                    pending.push(original);
                }
                for action in rules
                    .into_iter()
                    .rev()
                    .flat_map(|rule| rule.definition.actions.into_iter().rev())
                {
                    pending.push(match action {
                        Statement::Insert(action) => Pending::Relation(
                            action.table,
                            action.target_relation_bound,
                            RuleEvent::Insert,
                        ),
                        Statement::Update(action) => Pending::Relation(
                            action.table,
                            action.target_relation_bound,
                            RuleEvent::Update,
                        ),
                        Statement::Delete(action) => Pending::Relation(
                            action.table,
                            action.target_relation_bound,
                            RuleEvent::Delete,
                        ),
                        _ => Pending::Query,
                    });
                }
            }
        }
        if count > 1 {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "WITH cannot be used in a query that is rewritten by rules into multiple queries".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
