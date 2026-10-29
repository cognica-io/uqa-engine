//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT rule shape validation copies only deferred relation names from borrowed rule actions.

use super::{qualified_name, RuleCatalog, RuleEvent, SQLError};
use crate::{ast::Statement, plan::InsertPlan, production::Buffer};
use uqa_core::memory::{Produced, ProductionControl};

enum Pending {
    Relation(Produced<String>, bool, bool),
    Leave,
}
struct Action {
    invalid_set: bool,
    insert: Option<(Produced<String>, bool, bool)>,
}

pub fn validate_insert_rule_query_contract_with_control(
    catalog: &dyn RuleCatalog,
    table: &str,
    insert: &InsertPlan,
    mut original_target: impl FnMut(&str, RuleEvent) -> Result<Option<Produced<String>>, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut pending = Buffer::new(control);
    pending.push(
        Pending::Relation(
            control.copy_text(table)?,
            true,
            super::super::input_contract::conditional_input(insert),
        ),
        control,
    )?;
    let mut active = Buffer::<Produced<String>>::new(control);
    while let Some(next) = pending.pop() {
        crate::expr::check_statement_cancellation()?;
        control.check()?;
        let Pending::Relation(table, bound, conditional) = next else {
            active.pop();
            continue;
        };
        let table = catalog.resolve_mutation_target_with_control(&table, bound, control)?;
        let relation = catalog.resolve_rule_relation_with_control(&table, control)?;
        let table = qualified_name(&relation, control)?;
        if active.iter().any(|entry| entry.as_str() == table.as_str()) {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: format!(
                    "infinite recursion detected in rules for relation \"{}\"",
                    relation.name
                ),
            });
        }
        let mut survives = true;
        let mut has_instead = false;
        let mut actions = Buffer::new(control);
        catalog.visit_rules_with_control(
            &table,
            RuleEvent::Insert,
            true,
            control,
            &mut |rule| {
                survives &= !(rule.definition.instead && rule.definition.condition.is_none());
                has_instead |= rule.definition.instead;
                let conditional = conditional || rule.definition.condition.is_some();
                for action in rule.definition.actions.iter().rev() {
                    control.check()?;
                    let invalid_set = conditional
                        && super::super::action_binding::rule_action_has_set_operation(action);
                    let insert = match action {
                        Statement::Insert(insert) => Some((
                            control.copy_text(&insert.table)?,
                            insert.target_relation_bound,
                            conditional || insert.select_source.is_some() || insert.rows.len() > 1,
                        )),
                        _ => None,
                    };
                    actions.push(
                        Action {
                            invalid_set,
                            insert,
                        },
                        control,
                    )?;
                }
                Ok(())
            },
        )?;
        let original = if survives {
            original_target(&table, RuleEvent::Insert)?
        } else {
            None
        };
        active.push(table, control)?;
        pending.push(Pending::Leave, control)?;
        if let Some(target) = original {
            pending.push(
                Pending::Relation(target, true, conditional || has_instead),
                control,
            )?;
        }
        for action in actions {
            if action.invalid_set {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "conditional UNION/INTERSECT/EXCEPT statements are not implemented"
                        .into(),
                });
            }
            if let Some((table, bound, conditional)) = action.insert {
                pending.push(Pending::Relation(table, bound, conditional), control)?;
            }
        }
    }
    Ok(())
}
