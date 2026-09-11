//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger-name conflicts across current partition ancestry and descendants.
use super::EventAnalysisContext;
use crate::{
    ast::TableHierarchy,
    catalog::events::{reads::EventCatalogReads, StoredTrigger},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub trait EventPartitionCatalog {
    fn contains_loaded_table(&self, relation: &RelationIdentity) -> bool;
    fn table_names(&self) -> Result<Vec<String>, String>;
    fn try_table_hierarchy(&self, name: &str) -> Result<TableHierarchy, String>;
}
#[derive(Clone, Copy)]
pub struct EventLookupContext<'a> {
    pub analysis: EventAnalysisContext<'a>,
    pub partitions: &'a dyn EventPartitionCatalog,
    pub registry: &'a dyn EventCatalogReads,
}
impl EventLookupContext<'_> {
    pub fn partition_trigger_sources(
        &self,
        table: &str,
    ) -> Result<Vec<RelationIdentity>, SQLError> {
        let mut current = self.analysis.resolve_trigger_table(table)?;
        let mut sources = vec![current.clone()];
        if !self.partitions.contains_loaded_table(&current) {
            return Ok(sources);
        }
        loop {
            let hierarchy = self
                .partitions
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
    pub fn ensure_partition_trigger_name_available(
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
            .partitions
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
        let triggers = self.registry.read_triggers();
        for source in ancestor_sources {
            if triggers
                .get(&source)
                .is_some_and(|entries| entries.contains_key(name))
            {
                return Err(super::duplicate_object(
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
                return Err(super::duplicate_object(
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
            return Err(super::duplicate_object(
                "trigger",
                name,
                &relation.qualified_name(),
            ));
        }
        Ok(())
    }
    pub fn rule_privilege_subject(&self, table: &str) -> Result<String, SQLError> {
        let relation = self.analysis.resolve_rule_relation(table)?;
        self.analysis
            .catalog
            .event_relation_owner(&relation)
            .map(|(owner, _)| owner)
    }
    pub fn constraint_trigger_by_constraint_name(
        &self,
        table: &str,
        name: &str,
    ) -> Result<Option<StoredTrigger>, SQLError> {
        let relation = self.analysis.resolve_trigger_table(table)?;
        Ok(self
            .registry
            .read_triggers()
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
}
