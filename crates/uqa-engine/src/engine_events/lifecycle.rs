//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation and column lifecycle handling for stored triggers.

use std::collections::BTreeMap;

use uqa_sql::ast::{DropRule, DropTrigger, Expr};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use crate::engine_capabilities::RelationLookupMode;
use crate::{Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

impl Engine {
    pub(crate) fn rules_depending_on_relations(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        let targets = relations
            .iter()
            .map(|relation| {
                RelationIdentity::from_legacy_name(relation).map_err(StorageBackendError::Other)
            })
            .collect::<StorageBackendResult<std::collections::BTreeSet<_>>>()?;
        let rules = self.durable.rules.read();
        let mut dependents = Vec::new();
        for (event_relation, entries) in rules.iter() {
            if targets.contains(event_relation) {
                continue;
            }
            for rule in entries.values() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ))
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

    pub(crate) fn drop_rules_depending_on_relations_inner(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<()> {
        let dependents = self.rules_depending_on_relations(relations)?;
        if dependents.is_empty() {
            return Ok(());
        }
        let mut rules = self.durable.rules.write();
        let mut next = rules.clone();
        for (event_relation, name) in &dependents {
            let removed = next
                .get_mut(event_relation)
                .and_then(|entries| entries.remove(name));
            if removed.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "dependent rule `{name}` on `{}` disappeared after DROP preflight",
                    event_relation.qualified_name()
                )));
            }
            if next.get(event_relation).is_some_and(BTreeMap::is_empty) {
                next.remove(event_relation);
            }
        }
        self.persist_rule_catalog_snapshot(&next)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *rules = next;
        drop(rules);
        for (event_relation, name) in dependents {
            self.push_sql_notice(
                "NOTICE",
                &format!(
                    "drop cascades to rule {name} on table {}",
                    event_relation.qualified_name()
                ),
            );
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rules_depending_on_routine(
        &self,
        name: &str,
        argument_types: &[String],
    ) -> StorageBackendResult<Vec<(RelationIdentity, String)>> {
        let rules = self.durable.rules.read();
        let mut dependents = Vec::new();
        for (event_relation, entries) in rules.iter() {
            for rule in entries.values() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ))
                })?;
                if dependencies.routines.iter().any(|dependency| {
                    dependency.name == name && dependency.argument_types == argument_types
                }) {
                    dependents.push((event_relation.clone(), rule.definition.name.clone()));
                }
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    pub(crate) fn drop_relation_events_inner(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        let mut rules = self.durable.rules.write();
        let qualified = relation.qualified_name();
        let referenced_by_trigger = triggers.values().any(|entries| {
            entries.values().any(|trigger| {
                trigger.definition.referenced_table.as_deref() == Some(qualified.as_str())
            })
        });
        if !triggers.contains_key(relation)
            && !rules.contains_key(relation)
            && !referenced_by_trigger
        {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        let mut removed_constraint_identities = Vec::new();
        for (trigger_relation, entries) in triggers.iter() {
            for trigger in entries.values() {
                if trigger.definition.constraint
                    && (trigger_relation == relation
                        || trigger.definition.referenced_table.as_deref()
                            == Some(qualified.as_str()))
                {
                    removed_constraint_identities.push(
                        Self::constraint_trigger_identity(trigger).map_err(|error| {
                            StorageBackendError::Other(format!(
                                "resolve dropped constraint-trigger identity: {error}"
                            ))
                        })?,
                    );
                }
            }
        }
        next_triggers.remove(relation);
        for entries in next_triggers.values_mut() {
            entries.retain(|_, trigger| {
                trigger.definition.referenced_table.as_deref() != Some(qualified.as_str())
            });
        }
        next_triggers.retain(|_, entries| !entries.is_empty());
        next_rules.remove(relation);
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next_triggers;
        *rules = next_rules;
        drop(rules);
        drop(triggers);
        for identity in &removed_constraint_identities {
            self.forget_constraint_trigger_events(identity);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_relation_events_inner(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.durable.triggers.write();
        let mut rules = self.durable.rules.write();
        let from_name = from.qualified_name();
        let to_name = to.qualified_name();
        let referenced_by_trigger = triggers.values().any(|entries| {
            entries.values().any(|trigger| {
                trigger.definition.referenced_table.as_deref() == Some(from_name.as_str())
            })
        });
        let mut referenced_by_rule = false;
        for (event_relation, entries) in rules.iter() {
            for rule in entries.values() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ))
                })?;
                referenced_by_rule |= dependencies.relations.contains(from);
            }
        }
        if !triggers.contains_key(from)
            && !rules.contains_key(from)
            && !referenced_by_trigger
            && !referenced_by_rule
        {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        if let Some(mut entries) = next_triggers.remove(from) {
            for trigger in entries.values_mut() {
                trigger.definition.table.clone_from(&to_name);
            }
            next_triggers.insert(to.clone(), entries);
        }
        for entries in next_triggers.values_mut() {
            for trigger in entries.values_mut() {
                if trigger.definition.referenced_table.as_deref() == Some(from_name.as_str()) {
                    trigger.definition.referenced_table = Some(to_name.clone());
                }
            }
        }
        if let Some(mut entries) = next_rules.remove(from) {
            for rule in entries.values_mut() {
                rule.definition.table = to.qualified_name();
            }
            next_rules.insert(to.clone(), entries);
        }
        for entries in next_rules.values_mut() {
            for rule in entries.values_mut() {
                super::rule_dependencies::rewrite_stored_rule_relation(rule, from, to).map_err(
                    |error| {
                        StorageBackendError::Other(format!(
                            "rewrite rule `{}` relation dependency: {error}",
                            rule.definition.name
                        ))
                    },
                )?;
            }
        }
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *triggers = next_triggers;
        *rules = next_rules;
        drop(rules);
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_event_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let triggers = self.durable.triggers.read().clone();
        let rules = self.durable.rules.read().clone();
        let dependency = super::RuleColumnDependency {
            relation: relation.clone(),
            column: from.to_string(),
        };
        let referenced_by_rule = rules.values().any(|entries| {
            entries.values().any(|rule| {
                rule.dependencies
                    .as_ref()
                    .is_some_and(|dependencies| dependencies.columns.contains(&dependency))
            })
        });
        if !triggers.contains_key(&relation)
            && !rules.contains_key(&relation)
            && !referenced_by_rule
        {
            return Ok(());
        }
        let mut next_triggers = triggers.clone();
        let mut next_rules = rules.clone();
        if let Some(entries) = next_triggers.get_mut(&relation) {
            for trigger in entries.values_mut() {
                for column in &mut trigger.definition.update_columns {
                    if column == from {
                        *column = to.to_string();
                    }
                }
                if let Some(condition) = trigger.definition.when.as_mut() {
                    crate::engine_table_storage::rename_schema_expr_column(condition, from, to)?;
                }
            }
        }
        self.rewrite_rule_catalog_column(&mut next_rules, &dependency, &relation, from, to)?;
        self.persist_trigger_catalog_snapshot(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.persist_rule_catalog_snapshot(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *self.durable.triggers.write() = next_triggers;
        *self.durable.rules.write() = next_rules;
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn rewrite_rule_catalog_column(
        &self,
        rules: &mut BTreeMap<RelationIdentity, BTreeMap<String, super::StoredRule>>,
        dependency: &super::RuleColumnDependency,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        for (event_relation, entries) in rules {
            for rule in entries.values_mut() {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ))
                })?;
                if dependencies.columns.contains(dependency) {
                    self.rewrite_stored_rule_column(rule, event_relation, relation, from, to)?;
                }
            }
        }
        Ok(())
    }

    fn rewrite_stored_rule_column(
        &self,
        rule: &mut super::StoredRule,
        event_relation: &RelationIdentity,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        if event_relation == relation {
            self.rewrite_rule_event_row_column(rule, from, to)?;
        }
        self.rewrite_rule_column_references(&mut rule.definition, relation, from, to)
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "rewrite rule `{}` column dependency: {error}",
                    rule.definition.name
                ))
            })?;
        let (validated_relation, condition_plan, condition_binding, dependencies) = self
            .validate_rule_definition(&mut rule.definition, RelationLookupMode::Bound, None, None)
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "rebind rule `{}` after column rename: {error}",
                    rule.definition.name
                ))
            })?;
        if validated_relation != *event_relation {
            return Err(StorageBackendError::Other(format!(
                "rule `{}` changed event relation while rebinding column rename",
                rule.definition.name
            )));
        }
        rule.condition_plan = condition_plan;
        rule.condition_binding = condition_binding;
        rule.dependencies = Some(dependencies);
        Ok(())
    }

    fn rewrite_rule_event_row_column(
        &self,
        rule: &mut super::StoredRule,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        if let Some(condition) = rule.definition.condition.as_mut() {
            *condition = super::bind_rule_expr_scoped(
                condition,
                &mut RuleColumnResolver {
                    from,
                    to: Some(to),
                    referenced: false,
                },
                &std::collections::BTreeSet::new(),
            )
            .map_err(|error| {
                StorageBackendError::Other(format!("rename rule condition column: {error}"))
            })?;
        }
        for action in &mut rule.definition.actions {
            let action_columns = self.rule_action_target_columns(action).map_err(|error| {
                StorageBackendError::Other(format!(
                    "read rule action columns during rename: {error}"
                ))
            })?;
            *action = super::bind_rule_action(
                self,
                action,
                &action_columns,
                &mut RuleColumnResolver {
                    from,
                    to: Some(to),
                    referenced: false,
                },
            )
            .map_err(|error| {
                StorageBackendError::Other(format!("rename rule event column: {error}"))
            })?;
        }
        Ok(())
    }

    pub(crate) fn prepare_rule_column_drop(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<super::PreparedRuleColumnDrop> {
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let dependency = super::RuleColumnDependency {
            relation: relation.clone(),
            column: column.to_string(),
        };
        let mut rules = self.durable.rules.read().clone();
        let mut rebind = std::collections::BTreeSet::new();
        for (event_relation, entries) in &mut rules {
            for (name, rule) in entries {
                let dependencies = rule.dependencies.as_ref().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{}` on `{}` has no bound dependency state",
                        rule.definition.name,
                        event_relation.qualified_name()
                    ))
                })?;
                if dependencies.columns.contains(&dependency) {
                    return Err(StorageBackendError::Other(format!(
                        "cannot drop column {column} of table {table} because rule {} on {} depends on it",
                        rule.definition.name,
                        event_relation.qualified_name()
                    )));
                }
                if event_relation != &relation && !dependencies.relations.contains(&relation) {
                    continue;
                }
                self.remove_rule_source_column_aliases(&mut rule.definition, &dependency)
                    .map_err(|error| {
                        StorageBackendError::Other(format!(
                            "reshape rule `{}` source aliases before column drop: {error}",
                            rule.definition.name
                        ))
                    })?;
                rebind.insert((event_relation.clone(), name.clone()));
            }
        }
        Ok(super::PreparedRuleColumnDrop { rules, rebind })
    }

    pub(crate) fn finish_rule_column_drop(
        &self,
        mut prepared: super::PreparedRuleColumnDrop,
    ) -> StorageBackendResult<()> {
        if prepared.rebind.is_empty() {
            return Ok(());
        }
        for (event_relation, name) in &prepared.rebind {
            let rule = prepared
                .rules
                .get_mut(event_relation)
                .and_then(|entries| entries.get_mut(name))
                .ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "rule `{name}` on `{}` disappeared while dropping a column",
                        event_relation.qualified_name()
                    ))
                })?;
            let (validated_relation, condition_plan, condition_binding, dependencies) = self
                .validate_rule_definition(
                    &mut rule.definition,
                    RelationLookupMode::Bound,
                    None,
                    None,
                )
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "rebind rule `{}` after column drop: {error}",
                        rule.definition.name
                    ))
                })?;
            if validated_relation != *event_relation {
                return Err(StorageBackendError::Other(format!(
                    "rule `{}` changed event relation while rebinding column drop",
                    rule.definition.name
                )));
            }
            rule.condition_plan = condition_plan;
            rule.condition_binding = condition_binding;
            rule.dependencies = Some(dependencies);
        }
        self.persist_rule_catalog_snapshot(&prepared.rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        *self.durable.rules.write() = prepared.rules;
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn handle_drop_column_event_dependencies(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{table}`: {error}"))
        })?;
        let dependent_triggers = self
            .durable
            .triggers
            .read()
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
                        crate::engine_table_storage::schema_expr_references_column(
                            condition, column,
                        )
                    })
            })
            .map(|trigger| trigger.definition.name.clone())
            .collect::<Vec<_>>();
        let rules = self.durable.rules.read();
        let dependent_rules = dependent_rules_for_column(&rules, &relation, column)?;
        drop(rules);
        if dependent_triggers.is_empty() && dependent_rules.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut objects = dependent_triggers
                .iter()
                .map(|name| format!("trigger {name}"))
                .collect::<Vec<_>>();
            objects.extend(
                dependent_rules
                    .iter()
                    .map(|(_, name)| format!("rule {name}")),
            );
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop column {column} of table {table} because {} depends on it",
                    objects.join(", ")
                ),
            });
        }
        for name in dependent_triggers {
            self.drop_trigger(&DropTrigger {
                name: name.clone(),
                table: table.to_string(),
                if_exists: false,
                cascade: true,
            })?;
            self.push_sql_notice(
                "NOTICE",
                &format!("drop cascades to trigger {name} on table {table}"),
            );
        }
        for (event_relation, name) in dependent_rules {
            let event_table = event_relation.qualified_name();
            self.drop_rule(&DropRule {
                name: name.clone(),
                table: event_table.clone(),
                if_exists: false,
                cascade: true,
            })?;
            self.push_sql_notice(
                "NOTICE",
                &format!("drop cascades to rule {name} on table {event_table}"),
            );
        }
        Ok(())
    }
}

fn dependent_rules_for_column(
    rules: &BTreeMap<RelationIdentity, BTreeMap<String, super::StoredRule>>,
    relation: &RelationIdentity,
    column: &str,
) -> Result<Vec<(RelationIdentity, String)>, SQLError> {
    let dependency = super::RuleColumnDependency {
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

struct RuleColumnResolver<'a> {
    from: &'a str,
    to: Option<&'a str>,
    referenced: bool,
}

impl VariableResolver for RuleColumnResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        if (qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"))
            && column == self.from
        {
            self.referenced = true;
            if let Some(to) = self.to {
                return Ok(Some(Expr::qualified_column(qualifier, to)));
            }
        }
        Ok(None)
    }
}
