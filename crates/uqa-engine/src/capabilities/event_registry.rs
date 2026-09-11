//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained event registries and pending constraint-event state for native lifecycle execution.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::events::context::{ConstraintTriggerEvents, EventLifecycleContext};
use uqa_sql::{
    ast::TableHierarchy,
    catalog::{
        constraints::ConstraintIdentity,
        events::{
            definition::lookup::{EventLookupContext, EventPartitionCatalog},
            reads::{EventCatalogReads, RuleCatalogRead, TriggerCatalogRead},
        },
    },
};

impl Engine {
    pub(crate) fn event_lookup_context(&self) -> EventLookupContext<'_> {
        EventLookupContext {
            analysis: self.event_analysis_context(),
            partitions: self,
            registry: self,
        }
    }
    pub(crate) fn event_lifecycle_context(&self) -> EventLifecycleContext<'_> {
        EventLifecycleContext {
            lookup: self.event_lookup_context(),
            catalog: self.event_catalog_context(),
            writer: self,
            notices: &self.runtime.notices,
            views: self,
            projection: self.catalog_execution(),
            pending: self,
        }
    }
}
impl EventCatalogReads for Engine {
    fn read_rules(&self) -> RuleCatalogRead<'_> {
        Box::new(self.durable.rules.read())
    }
    fn read_triggers(&self) -> TriggerCatalogRead<'_> {
        Box::new(self.durable.triggers.read())
    }
}
impl EventPartitionCatalog for Engine {
    fn contains_loaded_table(&self, relation: &RelationIdentity) -> bool {
        self.storage.tables.read().contains_key(relation)
    }
    fn table_names(&self) -> Result<Vec<String>, String> {
        Engine::table_names(self).map_err(|error| error.to_string())
    }
    fn try_table_hierarchy(&self, name: &str) -> Result<TableHierarchy, String> {
        Engine::try_table_hierarchy(self, name).map_err(|error| error.to_string())
    }
}
impl ConstraintTriggerEvents for Engine {
    fn forget(&self, identity: &ConstraintIdentity) {
        self.forget_constraint_trigger_events(identity);
    }
    fn rename_trigger(&self, identity: &ConstraintIdentity, name: &str) {
        self.rename_pending_constraint_trigger(identity, name);
    }
    fn rename_constraint(&self, identity: &ConstraintIdentity, name: &str) {
        self.rename_constraint_trigger_identity(identity, name);
    }
}

impl Engine {
    pub(crate) fn event_catalog_context(
        &self,
    ) -> uqa_execution::schema::events::EventCatalogContext<'_> {
        uqa_execution::schema::events::EventCatalogContext {
            registry: self,
            publication: self,
            changes: self,
        }
    }
}
impl uqa_execution::schema::events::EventCatalogGuards for Engine {
    fn triggers(&self) -> uqa_execution::schema::events::TriggerCatalogWrite<'_> {
        Box::new(self.durable.triggers.write())
    }
    fn rules(&self) -> uqa_execution::schema::events::RuleCatalogWrite<'_> {
        Box::new(self.durable.rules.write())
    }
}
impl uqa_execution::schema::events::EventCatalogPublication for Engine {
    fn persist_triggers(
        &self,
        triggers: &uqa_sql::catalog::events::TriggerCatalog,
    ) -> Result<(), uqa_sql::SQLError> {
        self.persist_trigger_catalog_snapshot(triggers)
    }
    fn persist_rules(
        &self,
        rules: &uqa_sql::catalog::events::RuleCatalog,
    ) -> Result<(), uqa_sql::SQLError> {
        self.persist_rule_catalog_snapshot(rules)
    }
}
