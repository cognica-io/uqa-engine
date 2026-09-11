//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound rule and trigger dependencies observed at each catalog read boundary.
use super::lookup::EventLookupContext;
use crate::{
    catalog::events::{PreparedRuleColumnDrop, RuleColumnDependency, StoredRule},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
pub struct EventColumnDependencies {
    pub triggers: Vec<String>,
    pub rules: Vec<(RelationIdentity, String)>,
}
impl EventLookupContext<'_> {
    pub fn rules_depending_on_relations(
        &self,
        relations: &[String],
    ) -> Result<Vec<(RelationIdentity, String)>, String> {
        let targets = relations
            .iter()
            .map(|relation| RelationIdentity::from_legacy_name(relation))
            .collect::<Result<std::collections::BTreeSet<_>, String>>()?;
        let rules = self.registry.read_rules();
        let mut dependents = Vec::new();
        for (event_relation, entries) in rules.iter() {
            if targets.contains(event_relation) {
                continue;
            }
            for rule in entries.values() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    )
                })?;
                if dependencies
                    .relations
                    .iter()
                    .any(|dependency| targets.contains(dependency))
                {
                    dependents.push((event_relation.clone(), rule.definition.name.clone()));
                }
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    pub fn rules_depending_on_routine(
        &self,
        target: &crate::ast::FunctionBinding,
    ) -> Result<Vec<(RelationIdentity, String)>, String> {
        let rules = self.registry.read_rules();
        let mut dependents = Vec::new();
        for (event_relation, entries) in rules.iter() {
            for rule in entries.values() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    )
                })?;
                if dependencies.routines.iter().any(|dependency| {
                    match (dependency.object_id, target.object_id) {
                        (Some(dependency), Some(target)) => dependency == target,
                        (None, None) => {
                            dependency.name == target.name
                                && dependency.argument_types == target.argument_types
                        }
                        _ => false,
                    }
                }) {
                    dependents.push((event_relation.clone(), rule.definition.name.clone()));
                }
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    pub fn triggers_depending_on_routine(
        &self,
        target: &crate::ast::FunctionBinding,
    ) -> Result<Vec<(String, String)>, SQLError> {
        let mut dependents = Vec::new();
        for trigger in self.list_triggers() {
            let invokes_target = match (trigger.function_object_id, target.object_id) {
                (Some(trigger), Some(target)) => trigger == target,
                (None, None) => {
                    target.argument_types.is_empty() && trigger.definition.function == target.name
                }
                _ => false,
            };
            let condition_references_target = trigger
                .definition
                .when
                .as_ref()
                .map(|condition| {
                    crate::catalog::stored_ast::expression_references_routine_identity(
                        condition, target,
                    )
                })
                .transpose()?
                .unwrap_or(false);
            if invokes_target || condition_references_target {
                dependents.push((
                    trigger.definition.table.clone(),
                    trigger.definition.name.clone(),
                ));
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    pub fn prepare_rule_column_drop(
        &self,
        table: &str,
        column: &str,
    ) -> Result<PreparedRuleColumnDrop, String> {
        let relation = RelationIdentity::from_legacy_name(table)?;
        let dependency = RuleColumnDependency {
            relation: relation.clone(),
            column: column.to_string(),
        };
        let mut rules = self.registry.read_rules().clone();
        let mut rebind = std::collections::BTreeSet::new();
        for (event_relation, entries) in &mut rules {
            for (name, rule) in entries {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    )
                })?;
                if dependencies.columns.contains(&dependency) {
                    return Err(format!(
                        "cannot drop column {column} of table {table} because rule {} on {} depends on it",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ));
                }
                if event_relation != &relation && !dependencies.relations.contains(&relation) {
                    continue;
                }
                crate::binding::stored_columns::remove_rule_source_column_aliases(
                    self.analysis.columns,
                    &mut rule.definition,
                    &dependency,
                )
                .map_err(|error| {
                    format!(
                        "reshape rule `{}` source aliases before column drop: {error}",
                        rule.definition.name
                    )
                })?;
                rebind.insert((event_relation.clone(), name.clone()));
            }
        }
        Ok(PreparedRuleColumnDrop { rules, rebind })
    }

    pub fn column_event_dependencies(
        &self,
        table: &str,
        column: &str,
    ) -> Result<EventColumnDependencies, SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{table}`: {error}"))
        })?;
        let dependent_triggers = self
            .registry
            .read_triggers()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|trigger| {
                trigger
                    .definition
                    .update_columns
                    .iter()
                    .any(|name| name == column)
                    || trigger.definition.when.as_ref().is_some_and(|condition| {
                        crate::schema::dependencies::schema_expr_references_column(
                            condition, column,
                        )
                    })
            })
            .map(|trigger| trigger.definition.name.clone())
            .collect::<Vec<_>>();
        let rules = self.registry.read_rules();
        let dependent_rules = dependent_rules_for_column(&rules, &relation, column)?;
        drop(rules);
        Ok(EventColumnDependencies {
            triggers: dependent_triggers,
            rules: dependent_rules,
        })
    }
}

fn dependent_rules_for_column(
    rules: &BTreeMap<RelationIdentity, BTreeMap<String, StoredRule>>,
    relation: &RelationIdentity,
    column: &str,
) -> Result<Vec<(RelationIdentity, String)>, SQLError> {
    let dependency = RuleColumnDependency {
        relation: relation.clone(),
        column: column.to_string(),
    };
    let mut dependent = Vec::new();
    for (event_relation, entries) in rules {
        for rule in entries.values() {
            let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "rule `{}` on `{}` has no bound dependency state",
                    rule.definition.name,
                    event_relation.qualified_name()
                ))
            })?;
            if dependencies.columns.contains(&dependency) {
                dependent.push((event_relation.clone(), rule.definition.name.clone()));
            }
        }
    }
    Ok(dependent)
}
