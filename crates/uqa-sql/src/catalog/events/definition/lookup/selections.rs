//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule and trigger selection over pinned query catalogs and live execution registries.

use std::collections::BTreeMap;

use crate::ast::{RuleEvent, TriggerEvent, TriggerTiming};
use crate::SQLError;

use super::EventLookupContext;

use crate::catalog::events::{StoredRule, StoredTrigger};

impl EventLookupContext<'_> {
    pub fn rule_definitions_for(
        &self,
        table: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError> {
        let relation = self.analysis.resolve_rule_relation(table)?;
        if let Some(snapshot) = self.state.query_rules() {
            return Ok(snapshot
                .get(&relation)
                .into_iter()
                .flat_map(BTreeMap::values)
                .filter(|rule| rule.definition.event == event)
                .cloned()
                .collect());
        }
        Ok(self
            .registry
            .read_rules()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|rule| rule.definition.event == event)
            .cloned()
            .collect())
    }

    pub fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        let relation = self.analysis.resolve_rule_relation(table)?;
        let replica = self.state.session_replication_role_is_replica();
        Ok(self
            .registry
            .read_rules()
            .get(&relation)
            .into_iter()
            .flat_map(BTreeMap::values)
            .filter(|rule| {
                (if replica {
                    rule.enabled.fires_in_replica()
                } else {
                    rule.enabled.fires_in_origin()
                }) && rule.definition.event == event
            })
            .cloned()
            .collect())
    }

    pub fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        let relation = self.analysis.resolve_rule_relation(table)?;
        Ok(self
            .registry
            .read_rules()
            .get(&relation)
            .is_some_and(|entries| !entries.is_empty()))
    }

    pub fn triggers_for(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        updated_columns: &[String],
    ) -> Result<Vec<StoredTrigger>, SQLError> {
        let relation = self.analysis.resolve_trigger_table(table)?;
        let replica = self.state.session_replication_role_is_replica();
        let relations = if row {
            self.partition_trigger_sources(&relation.qualified_name())?
        } else {
            vec![relation.clone()]
        };
        let triggers = self.registry.read_triggers();
        let mut candidates = BTreeMap::new();
        for source in relations {
            for trigger in triggers.get(&source).into_iter().flat_map(BTreeMap::values) {
                let mut trigger = trigger.clone();
                if source != relation {
                    trigger.definition.table = relation.qualified_name();
                }
                candidates
                    .entry(trigger.definition.name.clone())
                    .or_insert(trigger);
            }
        }
        Ok(candidates
            .into_values()
            .filter(|trigger| {
                (if replica {
                    trigger.enabled.fires_in_replica()
                } else {
                    trigger.enabled.fires_in_origin()
                }) && trigger.definition.timing == timing
                    && trigger.definition.row == row
                    && trigger.definition.events.contains(&event)
                    && (event != TriggerEvent::Update
                        || trigger.definition.update_columns.is_empty()
                        || trigger
                            .definition
                            .update_columns
                            .iter()
                            .any(|column| updated_columns.contains(column)))
            })
            .collect())
    }

    pub fn has_trigger_definition(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError> {
        let relation = self.analysis.resolve_trigger_table(table)?;
        let matches = |entries: &BTreeMap<String, StoredTrigger>| {
            entries.values().any(|trigger| {
                trigger.definition.timing == timing
                    && trigger.definition.row == row
                    && trigger.definition.events.contains(&event)
            })
        };
        if let Some(snapshot) = self.state.query_triggers() {
            return Ok(snapshot.get(&relation).is_some_and(matches));
        }
        Ok(self
            .registry
            .read_triggers()
            .get(&relation)
            .is_some_and(matches))
    }

    pub fn has_row_triggers(&self, table: &str, event: TriggerEvent) -> Result<bool, SQLError> {
        let relation = self.analysis.resolve_trigger_table(table)?;
        let sources = self.partition_trigger_sources(&relation.qualified_name())?;
        let replica = self.state.session_replication_role_is_replica();
        let triggers = self.registry.read_triggers();
        Ok(sources.iter().any(|source| {
            triggers.get(source).is_some_and(|entries| {
                entries.values().any(|trigger| {
                    (if replica {
                        trigger.enabled.fires_in_replica()
                    } else {
                        trigger.enabled.fires_in_origin()
                    }) && trigger.definition.row
                        && trigger.definition.events.contains(&event)
                })
            })
        }))
    }

    pub fn list_triggers(&self) -> Vec<StoredTrigger> {
        if let Some(snapshot) = self.state.query_triggers() {
            return snapshot
                .values()
                .flat_map(BTreeMap::values)
                .cloned()
                .collect();
        }
        self.registry
            .read_triggers()
            .values()
            .flat_map(BTreeMap::values)
            .cloned()
            .collect()
    }
}
