//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional trigger registry mutations.

use std::collections::BTreeMap;

use uqa_sql::ast::{CreateRule, CreateTrigger, DropRule, DropTrigger, EventEnableMode};
use uqa_sql::SQLError;

use crate::engine_capabilities::{RelationLookupMode, RelationResolution};
use crate::{Engine, RelationIdentity, StoredViewKind};

use super::{duplicate_object, undefined_object, undefined_rule, StoredRule, StoredTrigger};

impl Engine {
    fn event_relation_owner(
        &self,
        relation: &RelationIdentity,
    ) -> Result<(String, &'static str), SQLError> {
        if let Some(table) = self.storage.tables.read().get(relation) {
            return Ok((table.role_owner(), "table"));
        }
        if let Some(view) = self.durable.views.read().get(relation) {
            return Ok((
                view.role_owner.clone(),
                match view.kind {
                    StoredViewKind::View => "view",
                    StoredViewKind::Materialized => "materialized view",
                },
            ));
        }
        if self.durable.foreign_tables.read().contains_key(relation) {
            let owner = self
                .durable
                .foreign_table_security
                .read()
                .get(relation)
                .map(|security| security.role_owner.clone())
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "foreign trigger relation `{}` has no security metadata",
                        relation.qualified_name()
                    ))
                })?;
            return Ok((owner, "foreign table"));
        }
        Err(SQLError::Internal(format!(
            "event relation `{}` disappeared after resolution",
            relation.qualified_name()
        )))
    }

    pub(in crate::engine_events) fn ensure_event_relation_owner(
        &self,
        relation: &RelationIdentity,
        error_kind: Option<&str>,
    ) -> Result<(), SQLError> {
        let (owner, relation_kind) = self.event_relation_owner(relation)?;
        if self.current_user_has_role_privileges(&owner) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of {} {}",
                error_kind.unwrap_or(relation_kind),
                relation.name
            ),
        })
    }

    fn visible_event_drop_resolution(
        &self,
        requested: &str,
        if_exists: bool,
    ) -> Result<Option<RelationResolution>, SQLError> {
        let resolution = self.resolve_visible_relation_kind(requested)?;
        match resolution {
            RelationResolution::MissingSchema(schema) if if_exists => {
                self.push_sql_notice(
                    "NOTICE",
                    &format!("schema \"{schema}\" does not exist, skipping"),
                );
                Ok(None)
            }
            RelationResolution::MissingRelation if if_exists => {
                self.push_sql_notice(
                    "NOTICE",
                    &format!("relation \"{requested}\" does not exist, skipping"),
                );
                Ok(None)
            }
            resolution => Ok(Some(resolution)),
        }
    }

    pub(crate) fn register_rule(&self, mut definition: CreateRule) -> Result<(), SQLError> {
        let relation =
            self.validate_rule_definition(&mut definition, RelationLookupMode::Dynamic)?;
        if definition.event == uqa_sql::ast::RuleEvent::Select {
            if !definition.or_replace {
                return Err(duplicate_object(
                    "rule",
                    &definition.name,
                    &definition.table,
                ));
            }
            let existing = self
                .view_definition(&definition.table)?
                .ok_or_else(|| SQLError::UnknownTable(definition.table.clone()))?;
            let action = definition.actions.into_iter().next().ok_or_else(|| {
                SQLError::Internal("validated ON SELECT rule lost its action".into())
            })?;
            let plan = uqa_planner::UnifiedPlan::lower_with(action, &|name: &str| {
                self.has_registered_aggregate_function(name)
            });
            let plan = crate::sql::optimize_engine_plan(self, plan)?;
            let uqa_planner::UnifiedPlan::Query(plan) = plan else {
                return Err(SQLError::Internal(
                    "ON SELECT rule action lowered to a command".into(),
                ));
            };
            let output_columns = existing.output_columns.unwrap_or_default();
            self.register_view_plan(crate::engine_session::ViewRegistration {
                name: &definition.table,
                column_names: &output_columns,
                plan: *plan,
                or_replace: true,
                persistence: existing.persistence,
                options: &existing.options,
                params: &[],
            })?;
            return Ok(());
        }
        self.prepare_explicit_transaction_writer()?;
        let mut rules = self.durable.rules.write();
        let mut next = rules.clone();
        let relation_rules = next.entry(relation).or_default();
        if relation_rules.contains_key(&definition.name) && !definition.or_replace {
            return Err(duplicate_object(
                "rule",
                &definition.name,
                &definition.table,
            ));
        }
        let enabled = relation_rules
            .get(&definition.name)
            .map_or(EventEnableMode::Origin, |rule| rule.enabled);
        relation_rules.insert(
            definition.name.clone(),
            StoredRule {
                definition,
                enabled,
            },
        );
        self.persist_rule_catalog_snapshot(&next)?;
        *rules = next;
        drop(rules);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn drop_rule_sql(&self, statement: &DropRule) -> Result<(), SQLError> {
        let Some(resolution) =
            self.visible_event_drop_resolution(&statement.table, statement.if_exists)?
        else {
            return Ok(());
        };
        let (relation, _) = Self::event_relation_from_resolution(&statement.table, resolution)?;
        let mut bound = statement.clone();
        bound.table = relation.qualified_name();
        let rule_exists = self
            .durable
            .rules
            .read()
            .get(&relation)
            .is_some_and(|rules| rules.contains_key(&bound.name));
        if rule_exists {
            self.ensure_event_relation_owner(&relation, Some("relation"))?;
        }
        self.drop_rule(&bound)
    }

    pub(crate) fn drop_rule(&self, statement: &DropRule) -> Result<(), SQLError> {
        let relation = self.resolve_rule_relation(&statement.table)?;
        let table = relation.qualified_name();
        if statement.name == "_RETURN" && self.view_definition(&table)?.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop rule _RETURN on view {} because view {} requires it\nHINT: You can drop view {} instead.",
                    relation.name, relation.name, relation.name
                ),
            });
        }
        self.prepare_explicit_transaction_writer()?;
        let mut rules = self.durable.rules.write();
        let mut next = rules.clone();
        let removed = next
            .get_mut(&relation)
            .and_then(|entries| entries.remove(&statement.name));
        if removed.is_none() {
            if statement.if_exists {
                self.push_sql_notice(
                    "NOTICE",
                    &format!(
                        "rule \"{}\" for relation \"{}\" does not exist, skipping",
                        statement.name, table
                    ),
                );
                return Ok(());
            }
            return Err(undefined_rule(&statement.name, &table));
        }
        if next.get(&relation).is_some_and(BTreeMap::is_empty) {
            next.remove(&relation);
        }
        self.persist_rule_catalog_snapshot(&next)?;
        *rules = next;
        drop(rules);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_rule(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        let relation = self.resolve_rule_relation(table)?;
        self.ensure_event_relation_owner(&relation, None)?;
        let is_view = self.view_definition(&relation.qualified_name())?.is_some();
        if is_view && from == "_RETURN" {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "renaming an ON SELECT rule is not allowed".into(),
            });
        }
        if is_view && to == "_RETURN" {
            return Err(duplicate_object("rule", to, &relation.qualified_name()));
        }
        self.prepare_explicit_transaction_writer()?;
        let mut rules = self.durable.rules.write();
        let mut next = rules.clone();
        let entries = next.entry(relation).or_default();
        if entries.contains_key(to) {
            return Err(duplicate_object("rule", to, table));
        }
        let mut rule = entries
            .remove(from)
            .ok_or_else(|| undefined_rule(from, table))?;
        rule.definition.name = to.to_string();
        entries.insert(to.to_string(), rule);
        self.persist_rule_catalog_snapshot(&next)?;
        *rules = next;
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn set_rule_enable_mode(
        &self,
        table: &str,
        name: &str,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        let relation = self.resolve_rule_relation(table)?;
        self.ensure_event_relation_owner(&relation, None)?;
        self.prepare_explicit_transaction_writer()?;
        let mut rules = self.durable.rules.write();
        let mut next = rules.clone();
        next.entry(relation)
            .or_default()
            .get_mut(name)
            .ok_or_else(|| undefined_rule(name, table))?
            .enabled = mode;
        self.persist_rule_catalog_snapshot(&next)?;
        *rules = next;
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rule_privilege_subject(&self, table: &str) -> Result<String, SQLError> {
        let relation = self.resolve_rule_relation(table)?;
        self.event_relation_owner(&relation).map(|(owner, _)| owner)
    }

    pub(crate) fn register_trigger(&self, mut definition: CreateTrigger) -> Result<(), SQLError> {
        let relation =
            self.validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)?;
        self.ensure_partition_trigger_name_available(
            &relation,
            &definition.name,
            definition.or_replace,
        )?;
        self.prepare_explicit_transaction_writer()?;
        let mut triggers = self.durable.triggers.write();
        let mut next = triggers.clone();
        let table_triggers = next.entry(relation).or_default();
        if definition.or_replace
            && table_triggers
                .get(&definition.name)
                .is_some_and(|trigger| trigger.definition.constraint)
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "CREATE OR REPLACE CONSTRAINT TRIGGER is not supported".into(),
            });
        }
        if table_triggers.contains_key(&definition.name) && !definition.or_replace {
            return Err(duplicate_object(
                "trigger",
                &definition.name,
                &definition.table,
            ));
        }
        let object_id = match table_triggers.get(&definition.name) {
            Some(trigger) => trigger.object_id,
            None => Some(new_trigger_object_id()?),
        };
        let constraint_name = definition.constraint.then(|| definition.name.clone());
        table_triggers.insert(
            definition.name.clone(),
            StoredTrigger {
                definition,
                enabled: EventEnableMode::Origin,
                object_id,
                constraint_name,
            },
        );
        self.persist_trigger_catalog_snapshot(&next)?;
        *triggers = next;
        drop(triggers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn drop_trigger_sql(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        let Some(resolution) =
            self.visible_event_drop_resolution(&statement.table, statement.if_exists)?
        else {
            return Ok(());
        };
        let (relation, _) = Self::trigger_relation_from_resolution(&statement.table, resolution)?;
        let mut bound = statement.clone();
        bound.table = relation.qualified_name();
        let trigger_exists = {
            let triggers = self.durable.triggers.read();
            triggers
                .get(&relation)
                .is_some_and(|triggers| triggers.contains_key(&bound.name))
        };
        if trigger_exists {
            self.ensure_event_relation_owner(&relation, Some("relation"))?;
        }
        self.drop_trigger(&bound)
    }

    fn ensure_partition_trigger_name_available(
        &self,
        relation: &RelationIdentity,
        name: &str,
        replacing_local: bool,
    ) -> Result<(), SQLError> {
        let ancestor_sources = self
            .partition_trigger_sources(&relation.qualified_name())?
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>();
        let mut descendant_relations = Vec::new();
        for table in self
            .table_names()
            .map_err(|error| SQLError::Internal(format!("read trigger partitions: {error}")))?
        {
            if table == relation.qualified_name() {
                continue;
            }
            let sources = self.partition_trigger_sources(&table)?;
            if sources.iter().skip(1).any(|source| source == relation) {
                descendant_relations.push(RelationIdentity::from_legacy_name(&table).map_err(
                    |error| {
                        SQLError::Internal(format!("decode trigger partition `{table}`: {error}"))
                    },
                )?);
            }
        }
        let triggers = self.durable.triggers.read();
        for source in ancestor_sources {
            if triggers
                .get(&source)
                .is_some_and(|entries| entries.contains_key(name))
            {
                return Err(duplicate_object(
                    "trigger",
                    name,
                    &relation.qualified_name(),
                ));
            }
        }
        for descendant in descendant_relations {
            if triggers
                .get(&descendant)
                .is_some_and(|entries| entries.contains_key(name))
            {
                return Err(duplicate_object(
                    "trigger",
                    name,
                    &descendant.qualified_name(),
                ));
            }
        }
        if !replacing_local
            && triggers
                .get(relation)
                .is_some_and(|entries| entries.contains_key(name))
        {
            return Err(duplicate_object(
                "trigger",
                name,
                &relation.qualified_name(),
            ));
        }
        Ok(())
    }

    pub(crate) fn drop_trigger(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        let relation = self.resolve_trigger_table(&statement.table)?;
        let table = relation.qualified_name();
        self.prepare_explicit_transaction_writer()?;
        let mut triggers = self.durable.triggers.write();
        let mut next = triggers.clone();
        let removed = next
            .get_mut(&relation)
            .and_then(|entries| entries.remove(&statement.name));
        let Some(removed) = removed else {
            if statement.if_exists {
                self.push_sql_notice(
                    "NOTICE",
                    &format!(
                        "trigger \"{}\" for relation \"{}\" does not exist, skipping",
                        statement.name, table
                    ),
                );
                return Ok(());
            }
            return Err(undefined_object("trigger", &statement.name, &relation.name));
        };
        if next.get(&relation).is_some_and(BTreeMap::is_empty) {
            next.remove(&relation);
        }
        self.persist_trigger_catalog_snapshot(&next)?;
        *triggers = next;
        drop(triggers);
        if removed.definition.constraint {
            self.forget_constraint_trigger_events(&Self::constraint_trigger_identity(&removed)?);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn rename_trigger(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        self.ensure_event_relation_owner(&relation, None)?;
        self.prepare_explicit_transaction_writer()?;
        let mut triggers = self.durable.triggers.write();
        let mut next = triggers.clone();
        let entries = next.entry(relation).or_default();
        if entries.contains_key(to) {
            return Err(duplicate_object("trigger", to, table));
        }
        let mut trigger = entries
            .remove(from)
            .ok_or_else(|| undefined_object("trigger", from, table))?;
        let constraint_identity = trigger
            .definition
            .constraint
            .then(|| Self::constraint_trigger_identity(&trigger))
            .transpose()?;
        trigger.definition.name = to.to_string();
        entries.insert(to.to_string(), trigger);
        self.persist_trigger_catalog_snapshot(&next)?;
        *triggers = next;
        drop(triggers);
        if let Some(identity) = constraint_identity.as_ref() {
            self.rename_pending_constraint_trigger(identity, to);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn constraint_trigger_by_constraint_name(
        &self,
        table: &str,
        name: &str,
    ) -> Result<Option<StoredTrigger>, SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        Ok(self
            .durable
            .triggers
            .read()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .find(|trigger| {
                trigger.definition.constraint
                    && trigger
                        .constraint_name
                        .as_deref()
                        .unwrap_or(&trigger.definition.name)
                        == name
            })
            .cloned())
    }

    pub(crate) fn rename_trigger_constraint(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        if crate::sql::runtime_constraints(self)?
            .iter()
            .any(|constraint| {
                constraint.identity.relation == relation && constraint.identity.name == to
            })
        {
            return Err(SQLError::Routine {
                sqlstate: "42710".into(),
                message: format!(
                    "constraint \"{to}\" for relation \"{}\" already exists",
                    relation.name
                ),
            });
        }
        self.prepare_explicit_transaction_writer()?;
        let mut triggers = self.durable.triggers.write();
        let mut next = triggers.clone();
        let entries = next.entry(relation.clone()).or_default();
        let trigger = entries
            .values_mut()
            .find(|trigger| {
                trigger.definition.constraint
                    && trigger
                        .constraint_name
                        .as_deref()
                        .unwrap_or(&trigger.definition.name)
                        == from
            })
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!(
                    "constraint \"{from}\" of relation \"{}\" does not exist",
                    relation.name
                ),
            })?;
        let old_identity = Self::constraint_trigger_identity(trigger)?;
        trigger.constraint_name = Some(to.to_string());
        self.persist_trigger_catalog_snapshot(&next)?;
        *triggers = next;
        drop(triggers);
        self.rename_constraint_trigger_identity(&old_identity, to);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn set_trigger_enable_mode(
        &self,
        table: &str,
        name: Option<&str>,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        self.prepare_explicit_transaction_writer()?;
        let mut triggers = self.durable.triggers.write();
        let mut next = triggers.clone();
        let entries = next.entry(relation).or_default();
        if let Some(name) = name {
            entries
                .get_mut(name)
                .ok_or_else(|| undefined_object("trigger", name, table))?
                .enabled = mode;
        } else {
            for trigger in entries.values_mut() {
                trigger.enabled = mode;
            }
        }
        self.persist_trigger_catalog_snapshot(&next)?;
        *triggers = next;
        self.note_catalog_registry_changed();
        Ok(())
    }
}

fn new_trigger_object_id() -> Result<[u8; 16], SQLError> {
    let mut object_id = [0_u8; 16];
    getrandom::fill(&mut object_id).map_err(|error| {
        SQLError::Internal(format!(
            "allocate constraint-trigger object identity: {error}"
        ))
    })?;
    if object_id == [0; 16] {
        object_id[15] = 1;
    }
    Ok(object_id)
}
