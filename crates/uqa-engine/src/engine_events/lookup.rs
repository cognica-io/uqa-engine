//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger lookup across ordinary and partitioned relations.

use std::collections::BTreeMap;

use uqa_sql::ast::{RuleEvent, TriggerEvent, TriggerTiming};
use uqa_sql::SQLError;

use crate::{Engine, RelationIdentity};

use super::{StoredRule, StoredTrigger};

impl Engine {
    pub(crate) fn rules_for(
        &self,
        table: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError> {
        let relation = self.resolve_rule_relation(table)?;
        let replica = self.session_replication_role_is_replica();
        Ok(self
            .durable
            .rules
            .read()
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

    pub(crate) fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        let relation = self.resolve_rule_relation(table)?;
        Ok(self
            .durable
            .rules
            .read()
            .get(&relation)
            .is_some_and(|entries| !entries.is_empty()))
    }

    pub(crate) fn query_relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        let (Some(tables), Some(catalog)) = (
            self.query_table_snapshots.as_ref(),
            self.query_catalog_snapshot.as_ref(),
        ) else {
            return self.relation_has_rules(table);
        };
        let relation = self
            .relation_lookup_candidates(table)
            .map_err(|error| {
                SQLError::Internal(format!("resolve query rule table `{table}`: {error}"))
            })?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        Ok(catalog
            .rules
            .get(&relation)
            .is_some_and(|entries| !entries.is_empty()))
    }

    pub(crate) fn list_rules(&self) -> Vec<StoredRule> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return snapshot
                .rules
                .values()
                .flat_map(BTreeMap::values)
                .cloned()
                .collect();
        }
        self.durable
            .rules
            .read()
            .values()
            .flat_map(BTreeMap::values)
            .cloned()
            .collect()
    }

    pub(crate) fn triggers_for(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        updated_columns: &[String],
    ) -> Result<Vec<StoredTrigger>, SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        let replica = self.session_replication_role_is_replica();
        let relations = if row {
            self.partition_trigger_sources(&relation.qualified_name())?
        } else {
            vec![relation.clone()]
        };
        let triggers = self.durable.triggers.read();
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

    pub(crate) fn has_row_triggers(
        &self,
        table: &str,
        event: TriggerEvent,
    ) -> Result<bool, SQLError> {
        let relation = self.resolve_trigger_table(table)?;
        let sources = self.partition_trigger_sources(&relation.qualified_name())?;
        let replica = self.session_replication_role_is_replica();
        let triggers = self.durable.triggers.read();
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

    pub(crate) fn partition_trigger_sources(
        &self,
        table: &str,
    ) -> Result<Vec<RelationIdentity>, SQLError> {
        let mut current = self.resolve_trigger_table(table)?;
        let mut sources = vec![current.clone()];
        loop {
            let hierarchy = self
                .try_table_hierarchy(&current.qualified_name())
                .map_err(|error| {
                    SQLError::Internal(format!("read trigger partition hierarchy: {error}"))
                })?;
            if hierarchy.partition_bound.is_none() {
                break;
            }
            let Some(parent) = hierarchy.parents.first() else {
                return Err(SQLError::Internal(format!(
                    "partition `{}` has no parent",
                    current.qualified_name()
                )));
            };
            current = RelationIdentity::from_legacy_name(parent).map_err(|error| {
                SQLError::Internal(format!(
                    "decode trigger partition parent `{parent}`: {error}"
                ))
            })?;
            sources.push(current.clone());
        }
        Ok(sources)
    }

    pub(crate) fn relation_has_triggers(&self, table: &str) -> Result<bool, SQLError> {
        RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode trigger relation `{table}`: {error}"))
        })?;
        let sources = self.partition_trigger_sources(table)?;
        let triggers = self.durable.triggers.read();
        Ok(sources.iter().enumerate().any(|(index, source)| {
            triggers.get(source).is_some_and(|entries| {
                entries
                    .values()
                    .any(|trigger| index == 0 || trigger.definition.row)
            })
        }))
    }

    pub(crate) fn query_relation_has_triggers(&self, table: &str) -> Result<bool, SQLError> {
        let (Some(tables), Some(catalog)) = (
            self.query_table_snapshots.as_ref(),
            self.query_catalog_snapshot.as_ref(),
        ) else {
            return self.relation_has_triggers(table);
        };
        let mut current = self
            .relation_lookup_candidates(table)
            .map_err(|error| {
                SQLError::Internal(format!("resolve query trigger table `{table}`: {error}"))
            })?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let mut sources = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(SQLError::Internal(format!(
                    "trigger partition hierarchy contains a cycle at `{}`",
                    current.qualified_name()
                )));
            }
            sources.push(current.clone());
            let hierarchy = tables
                .get(&current)
                .ok_or_else(|| SQLError::UnknownTable(current.qualified_name()))?
                .hierarchy
                .read()
                .clone();
            if hierarchy.partition_bound.is_none() {
                break;
            }
            let Some(parent) = hierarchy.parents.first() else {
                return Err(SQLError::Internal(format!(
                    "partition `{}` has no parent",
                    current.qualified_name()
                )));
            };
            current = RelationIdentity::from_legacy_name(parent).map_err(|error| {
                SQLError::Internal(format!(
                    "decode query trigger partition parent `{parent}`: {error}"
                ))
            })?;
        }
        Ok(sources.iter().enumerate().any(|(index, source)| {
            catalog.triggers.get(source).is_some_and(|entries| {
                entries
                    .values()
                    .any(|trigger| index == 0 || trigger.definition.row)
            })
        }))
    }

    pub(crate) fn list_triggers(&self) -> Vec<StoredTrigger> {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return snapshot
                .triggers
                .values()
                .flat_map(BTreeMap::values)
                .cloned()
                .collect();
        }
        self.durable
            .triggers
            .read()
            .values()
            .flat_map(BTreeMap::values)
            .cloned()
            .collect()
    }
}
