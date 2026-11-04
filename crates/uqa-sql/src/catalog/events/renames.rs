//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite bound relation identities in rule actions, conditions, and event catalogs.
use super::{RuleCatalog, StoredRule, TriggerCatalog};
use crate::{ast::Statement, catalog::stored_ast::StoredAstVisitor, SQLError};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
pub fn rewrite_stored_statement_relation(
    statement: &mut Statement,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<bool, SQLError> {
    let from = from.qualified_name();
    let to = to.qualified_name();
    let mut changed = false;
    let mut rewrite = |reference: &mut String| {
        if reference == &from {
            reference.clone_from(&to);
            changed = true;
        }
        Ok(())
    };
    let mut ignore_routine = |_: &mut String,
                              _: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> { Ok(()) };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut rewrite,
        routine: &mut ignore_routine,
    }
    .bind_statement(statement)?;
    Ok(changed)
}

pub fn rewrite_stored_rule_relation(
    rule: &mut StoredRule,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<bool, SQLError> {
    let dependencies = rule.dependencies.as_mut().ok_or_else(|| {
        SQLError::Internal(format!(
            "rule `{}` has no bound dependency state",
            rule.definition.name
        ))
    })?;
    let mut changed = false;
    if dependencies.relations.remove(from) {
        dependencies.relations.insert(to.clone());
        changed = true;
    }
    let renamed_columns = dependencies
        .columns
        .iter()
        .filter(|dependency| &dependency.relation == from)
        .cloned()
        .collect::<Vec<_>>();
    for mut dependency in renamed_columns {
        dependencies.columns.remove(&dependency);
        dependency.relation = to.clone();
        dependencies.columns.insert(dependency);
        changed = true;
    }
    for action in &mut rule.definition.actions {
        changed |= rewrite_stored_statement_relation(action, from, to)?;
    }
    if let Some(condition) = &mut rule.definition.condition {
        let from_name = from.qualified_name();
        let to_name = to.qualified_name();
        let mut rewrite = |reference: &mut String| {
            if reference == &from_name {
                reference.clone_from(&to_name);
                changed = true;
            }
            Ok(())
        };
        let mut ignore_routine = |_: &mut String,
                                  _: Option<&mut Option<crate::ast::FunctionBinding>>|
         -> Result<(), SQLError> { Ok(()) };
        StoredAstVisitor {
            source: None,
            merge: None,
            expression: None,
            projection: None,
            ty: None,
            relation: &mut rewrite,
            routine: &mut ignore_routine,
        }
        .bind_expr(condition, &BTreeSet::new())?;
    }
    if let Some(plan) = &mut rule.condition_plan {
        for subquery in &mut plan.subqueries {
            crate::binding::view_dependencies::bind_query_plan_relations(
                subquery,
                &BTreeSet::new(),
                &mut |reference| -> Result<String, SQLError> {
                    let identity =
                        RelationIdentity::from_legacy_name(reference).map_err(|error| {
                            SQLError::Internal(format!(
                                "decode stored rule relation `{reference}`: {error}"
                            ))
                        })?;
                    if &identity == from {
                        changed = true;
                        Ok(to.qualified_name())
                    } else {
                        Ok(reference.to_string())
                    }
                },
            )?;
        }
    }
    Ok(changed)
}

pub fn renamed_relation_events(
    triggers: &TriggerCatalog,
    rules: &RuleCatalog,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> Result<Option<(TriggerCatalog, RuleCatalog)>, crate::catalog::events::EventChangeError> {
    let from_name = from.qualified_name();
    let to_name = to.qualified_name();
    let referenced_by_trigger = triggers.values().any(|entries| {
        entries.values().any(|trigger| {
            trigger.definition.referenced_table.as_deref() == Some(from_name.as_str())
        })
    });
    let mut referenced_by_rule = false;
    for (event_relation, entries) in rules {
        for rule in entries.values() {
            let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                format!(
                    "rule `{}` on `{}` has no bound dependency state",
                    rule.definition.name,
                    event_relation.qualified_name()
                )
            })?;
            referenced_by_rule |= dependencies.relations.contains(from);
        }
    }
    if !triggers.contains_key(from)
        && !rules.contains_key(from)
        && !referenced_by_trigger
        && !referenced_by_rule
    {
        return Ok(None);
    }
    let mut next_triggers = triggers.clone();
    let mut next_rules = rules.clone();
    if let Some(original) = triggers.get(from) {
        let mut entries = original.clone();
        for (name, trigger) in original {
            let mut trigger = trigger.clone();
            trigger.definition.table.clone_from(&to_name);
            entries.insert(name.clone(), trigger)?;
        }
        next_triggers.remove(from)?;
        next_triggers.insert(to.clone(), entries)?;
    }
    next_triggers.rewrite(|_, _, trigger| {
        if trigger.definition.referenced_table.as_deref() == Some(from_name.as_str()) {
            let mut trigger = trigger.clone();
            trigger.definition.referenced_table = Some(to_name.clone());
            return Ok(Some(trigger));
        }
        Ok(None)
    })?;
    if let Some(original) = rules.get(from) {
        let mut entries = original.clone();
        for (name, rule) in original {
            let mut rule = rule.clone();
            rule.definition.table = to.qualified_name();
            entries.insert(name.clone(), rule)?;
        }
        next_rules.remove(from)?;
        next_rules.insert(to.clone(), entries)?;
    }
    let original = next_rules.clone();
    for (relation, entries) in &original {
        let mut next = entries.clone();
        let mut changed = false;
        for (name, rule) in entries {
            let mut rule = rule.clone();
            if rewrite_stored_rule_relation(&mut rule, from, to).map_err(|error| {
                format!(
                    "rewrite rule `{}` relation dependency: {error}",
                    rule.definition.name
                )
            })? {
                next.insert(name.clone(), rule)?;
                changed = true;
            }
        }
        if changed {
            next_rules.insert(relation.clone(), next)?;
        }
    }
    Ok(Some((next_triggers, next_rules)))
}
