//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native rule and trigger registration, removal, rename, and enable-mode publication.
use super::context::EventLifecycleContext;
use std::collections::BTreeMap;
use uqa_sql::{
    ast::{CreateRule, CreateTrigger, DropRule, DropTrigger, EventEnableMode},
    catalog::{
        events::{
            definition::{duplicate_object, undefined_object, undefined_rule},
            StoredRule, StoredTrigger,
        },
        resolution::{RelationLookupMode, RelationResolution},
    },
    SQLError,
};

impl EventLifecycleContext<'_> {
    fn visible_event_drop_resolution(
        &self,
        requested: &str,
        if_exists: bool,
    ) -> Result<Option<RelationResolution>, SQLError> {
        let resolution = self
            .lookup
            .analysis
            .relations
            .resolve_visible_relation_kind(requested)?;
        match resolution {
            RelationResolution::MissingSchema(schema) if if_exists => {
                self.notice(
                    "NOTICE",
                    &format!("schema \"{schema}\" does not exist, skipping"),
                );
                Ok(None)
            }
            RelationResolution::MissingRelation if if_exists => {
                self.notice(
                    "NOTICE",
                    &format!("relation \"{requested}\" does not exist, skipping"),
                );
                Ok(None)
            }
            resolution => Ok(Some(resolution)),
        }
    }

    pub fn register_rule(&self, mut definition: CreateRule) -> Result<(), SQLError> {
        let (relation, condition_plan, condition_binding, dependencies) = self
            .lookup
            .analysis
            .validate_rule_definition(&mut definition, RelationLookupMode::Dynamic, None, None)?;
        if definition.event == uqa_sql::ast::RuleEvent::Select {
            if !definition.or_replace {
                return Err(duplicate_object(
                    "rule",
                    &definition.name,
                    &definition.table,
                ));
            }
            let existing = self
                .lookup
                .analysis
                .privileges
                .view_definition(&definition.table)?
                .ok_or_else(|| SQLError::UnknownTable(definition.table.clone()))?;
            let action = definition.actions.into_iter().next().ok_or_else(|| {
                SQLError::Internal("validated ON SELECT rule lost its action".into())
            })?;
            let plan = uqa_sql::plan::UnifiedPlan::lower_with(action, &|name: &str| {
                self.lookup
                    .analysis
                    .routines
                    .has_registered_aggregate_function(name)
            });
            let uqa_sql::plan::UnifiedPlan::Query(plan) = plan else {
                return Err(SQLError::Internal(
                    "ON SELECT rule action lowered to a command".into(),
                ));
            };
            let output_columns = existing.output_columns.unwrap_or_default();
            crate::schema::view_creation::register_view_plan(
                self.views,
                crate::schema::view_creation::ViewRegistration {
                    name: &definition.table,
                    column_names: &output_columns,
                    plan: *plan,
                    or_replace: true,
                    persistence: existing.persistence,
                    options: &existing.options,
                    params: &[],
                },
            )?;
            return Ok(());
        }
        self.writer.prepare_writer()?;
        let mut rules = self.catalog.registry.rules();
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
                condition_plan,
                condition_binding,
                dependencies: Some(dependencies),
            },
        );
        self.catalog.publication.persist_rules(&next)?;
        **rules = next;
        drop(rules);
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn drop_rule_sql(&self, statement: &DropRule) -> Result<(), SQLError> {
        let Some(resolution) =
            self.visible_event_drop_resolution(&statement.table, statement.if_exists)?
        else {
            return Ok(());
        };
        let (relation, _) = uqa_sql::catalog::events::definition::EventAnalysisContext::event_relation_from_resolution(&statement.table, resolution)?;
        let mut bound = statement.clone();
        bound.table = relation.qualified_name();
        let rule_exists = self
            .lookup
            .registry
            .read_rules()
            .get(&relation)
            .is_some_and(|rules| rules.contains_key(&bound.name));
        if rule_exists {
            self.lookup
                .analysis
                .ensure_event_relation_owner(&relation, Some("relation"))?;
        }
        self.drop_rule(&bound)
    }

    pub fn drop_rule(&self, statement: &DropRule) -> Result<(), SQLError> {
        let relation = self
            .lookup
            .analysis
            .resolve_rule_relation(&statement.table)?;
        let table = relation.qualified_name();
        if statement.name == "_RETURN"
            && self
                .lookup
                .analysis
                .privileges
                .view_definition(&table)?
                .is_some()
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop rule _RETURN on view {} because view {} requires it\nHINT: You can drop view {} instead.",
                    relation.name, relation.name, relation.name
                ),
            });
        }
        self.writer.prepare_writer()?;
        let mut rules = self.catalog.registry.rules();
        let mut next = rules.clone();
        let removed = next
            .get_mut(&relation)
            .and_then(|entries| entries.remove(&statement.name));
        if removed.is_none() {
            if statement.if_exists {
                self.notice(
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
        self.catalog.publication.persist_rules(&next)?;
        **rules = next;
        drop(rules);
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn rename_rule(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        let relation = self.lookup.analysis.resolve_rule_relation(table)?;
        self.lookup
            .analysis
            .ensure_event_relation_owner(&relation, None)?;
        let is_view = self
            .lookup
            .analysis
            .privileges
            .view_definition(&relation.qualified_name())?
            .is_some();
        if is_view && from == "_RETURN" {
            return Err(SQLError::Routine {
                sqlstate: "42P17".into(),
                message: "renaming an ON SELECT rule is not allowed".into(),
            });
        }
        if is_view && to == "_RETURN" {
            return Err(duplicate_object("rule", to, &relation.qualified_name()));
        }
        self.writer.prepare_writer()?;
        let mut rules = self.catalog.registry.rules();
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
        self.catalog.publication.persist_rules(&next)?;
        **rules = next;
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn set_rule_enable_mode(
        &self,
        table: &str,
        name: &str,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        let relation = self.lookup.analysis.resolve_rule_relation(table)?;
        self.lookup
            .analysis
            .ensure_event_relation_owner(&relation, None)?;
        self.writer.prepare_writer()?;
        let mut rules = self.catalog.registry.rules();
        let mut next = rules.clone();
        next.entry(relation)
            .or_default()
            .get_mut(name)
            .ok_or_else(|| undefined_rule(name, table))?
            .enabled = mode;
        self.catalog.publication.persist_rules(&next)?;
        **rules = next;
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn register_trigger(&self, mut definition: CreateTrigger) -> Result<(), SQLError> {
        let (relation, _) = self
            .lookup
            .analysis
            .validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)?;
        let function_object_id = self
            .lookup
            .analysis
            .resolve_trigger_function(&definition.function, RelationLookupMode::Bound)?
            .def
            .object_id
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "trigger function `{}` has no catalog object identity",
                    definition.function
                ))
            })?;
        self.lookup.ensure_partition_trigger_name_available(
            &relation,
            &definition.name,
            definition.or_replace,
        )?;
        self.writer.prepare_writer()?;
        let mut triggers = self.catalog.registry.triggers();
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
                function_object_id: Some(function_object_id),
                enabled: EventEnableMode::Origin,
                object_id,
                constraint_name,
            },
        );
        self.catalog.publication.persist_triggers(&next)?;
        **triggers = next;
        drop(triggers);
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn drop_trigger_sql(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        let Some(resolution) =
            self.visible_event_drop_resolution(&statement.table, statement.if_exists)?
        else {
            return Ok(());
        };
        let (relation, _) = uqa_sql::catalog::events::definition::EventAnalysisContext::trigger_relation_from_resolution(&statement.table, resolution)?;
        let mut bound = statement.clone();
        bound.table = relation.qualified_name();
        let trigger_exists = {
            let triggers = self.lookup.registry.read_triggers();
            triggers
                .get(&relation)
                .is_some_and(|triggers| triggers.contains_key(&bound.name))
        };
        if trigger_exists {
            self.lookup
                .analysis
                .ensure_event_relation_owner(&relation, Some("relation"))?;
        }
        self.drop_trigger(&bound)
    }

    pub fn drop_trigger(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        let relation = self
            .lookup
            .analysis
            .resolve_trigger_table(&statement.table)?;
        let table = relation.qualified_name();
        self.writer.prepare_writer()?;
        let mut triggers = self.catalog.registry.triggers();
        let mut next = triggers.clone();
        let removed = next
            .get_mut(&relation)
            .and_then(|entries| entries.remove(&statement.name));
        let Some(removed) = removed else {
            if statement.if_exists {
                self.notice(
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
        self.catalog.publication.persist_triggers(&next)?;
        **triggers = next;
        drop(triggers);
        if removed.definition.constraint {
            self.pending.forget(&removed.constraint_identity()?);
        }
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn rename_trigger(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        let relation = self.lookup.analysis.resolve_trigger_table(table)?;
        self.lookup
            .analysis
            .ensure_event_relation_owner(&relation, None)?;
        self.writer.prepare_writer()?;
        let mut triggers = self.catalog.registry.triggers();
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
            .then(|| trigger.constraint_identity())
            .transpose()?;
        trigger.definition.name = to.to_string();
        entries.insert(to.to_string(), trigger);
        self.catalog.publication.persist_triggers(&next)?;
        **triggers = next;
        drop(triggers);
        if let Some(identity) = constraint_identity.as_ref() {
            self.pending.rename_trigger(identity, to);
        }
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn rename_trigger_constraint(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        let relation = self.lookup.analysis.resolve_trigger_table(table)?;
        if crate::catalog::projection::runtime_constraints(&self.projection)?
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
        self.writer.prepare_writer()?;
        let mut triggers = self.catalog.registry.triggers();
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
        let old_identity = trigger.constraint_identity()?;
        trigger.constraint_name = Some(to.to_string());
        self.catalog.publication.persist_triggers(&next)?;
        **triggers = next;
        drop(triggers);
        self.pending.rename_constraint(&old_identity, to);
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn set_trigger_enable_mode(
        &self,
        table: &str,
        name: Option<&str>,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        let relation = self.lookup.analysis.resolve_trigger_table(table)?;
        self.writer.prepare_writer()?;
        let mut triggers = self.catalog.registry.triggers();
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
        self.catalog.publication.persist_triggers(&next)?;
        **triggers = next;
        self.catalog.changes.catalog_registry_changed();
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

impl EventLifecycleContext<'_> {
    pub(super) fn notice(&self, level: &str, message: &str) {
        self.notices
            .lock()
            .push((level.to_string(), message.to_string()));
    }
}
