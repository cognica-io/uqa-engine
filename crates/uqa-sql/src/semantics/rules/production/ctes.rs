//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WITH validation retains iterative rule expansion and its original action order.

use super::{qualified_name, RuleCatalog, RuleEvent, SQLError};
use crate::{ast::Statement, production::Buffer};
use uqa_core::memory::{Produced, ProductionControl};

enum Pending {
    Relation(Produced<String>, bool, RuleEvent),
    Query,
    Leave,
}

pub fn validate_rule_cte_contract_with_control(
    catalog: &dyn RuleCatalog,
    table: &str,
    event: RuleEvent,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<Produced<String>>, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut pending = Buffer::new(control);
    pending.push(
        Pending::Relation(control.copy_text(table)?, true, event),
        control,
    )?;
    let mut active = Buffer::<(Produced<String>, RuleEvent)>::new(control);
    let mut count = 0;
    while let Some(next) = pending.pop() {
        crate::expr::check_statement_cancellation()?;
        control.check()?;
        match next {
            Pending::Leave => {
                active.pop();
            }
            Pending::Query => count += 1,
            Pending::Relation(table, bound, event) => {
                let table = catalog.resolve_mutation_target_with_control(&table, bound, control)?;
                let relation = catalog.resolve_rule_relation_with_control(&table, control)?;
                let table = qualified_name(&relation, control)?;
                if active
                    .iter()
                    .any(|entry| entry.0.as_str() == table.as_str() && entry.1 == event)
                {
                    return Err(SQLError::Routine {
                        sqlstate: "42P17".into(),
                        message: format!(
                            "infinite recursion detected in rules for relation \"{}\"",
                            relation.name
                        ),
                    });
                }
                let mut survives = true;
                let mut actions = Buffer::new(control);
                catalog.visit_rules_with_control(&table, event, true, control, &mut |rule| {
                    survives &= !(rule.definition.instead && rule.definition.condition.is_none());
                    for action in rule.definition.actions.iter().rev() {
                        control.check()?;
                        let action = match action {
                            Statement::Insert(action) => Pending::Relation(
                                control.copy_text(&action.table)?,
                                action.target_relation_bound,
                                RuleEvent::Insert,
                            ),
                            Statement::Update(action) => Pending::Relation(
                                control.copy_text(&action.table)?,
                                action.target_relation_bound,
                                RuleEvent::Update,
                            ),
                            Statement::Delete(action) => Pending::Relation(
                                control.copy_text(&action.table)?,
                                action.target_relation_bound,
                                RuleEvent::Delete,
                            ),
                            _ => Pending::Query,
                        };
                        actions.push(action, control)?;
                    }
                    Ok(())
                })?;
                let original = if survives {
                    Some(
                        original_target(&table, event)?.map_or(Pending::Query, |target| {
                            Pending::Relation(target, true, event)
                        }),
                    )
                } else {
                    None
                };
                active.push((table, event), control)?;
                pending.push(Pending::Leave, control)?;
                if let Some(original) = original {
                    pending.push(original, control)?;
                }
                for action in actions {
                    pending.push(action, control)?;
                }
            }
        }
        if count > 1 {
            return Err(SQLError::Routine { sqlstate: "0A000".into(), message: "WITH cannot be used in a query that is rewritten by rules into multiple queries".into() });
        }
    }
    Ok(())
}
