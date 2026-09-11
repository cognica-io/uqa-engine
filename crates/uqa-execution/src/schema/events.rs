//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist event-catalog candidates under retained trigger and rule write guards.
use crate::schema::publication::dependencies::CatalogPublicationChanges;
use std::ops::DerefMut;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::events::{RuleCatalog, TriggerCatalog},
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};
pub type TriggerCatalogWrite<'a> = Box<dyn DerefMut<Target = TriggerCatalog> + 'a>;
pub type RuleCatalogWrite<'a> = Box<dyn DerefMut<Target = RuleCatalog> + 'a>;
pub trait EventCatalogGuards {
    fn triggers(&self) -> TriggerCatalogWrite<'_>;
    fn rules(&self) -> RuleCatalogWrite<'_>;
}
pub trait EventCatalogPublication {
    fn persist_triggers(&self, triggers: &TriggerCatalog) -> Result<(), SQLError>;
    fn persist_rules(&self, rules: &RuleCatalog) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct EventCatalogContext<'a> {
    pub registry: &'a dyn EventCatalogGuards,
    pub publication: &'a dyn EventCatalogPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn rename_relation_events(
    context: &EventCatalogContext<'_>,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> StorageBackendResult<()> {
    let mut triggers = context.registry.triggers();
    let mut rules = context.registry.rules();
    let Some((next_triggers, next_rules)) =
        uqa_sql::catalog::events::renames::renamed_relation_events(&triggers, &rules, from, to)
            .map_err(StorageBackendError::Other)?
    else {
        return Ok(());
    };
    context
        .publication
        .persist_triggers(&next_triggers)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    context
        .publication
        .persist_rules(&next_rules)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    **triggers = next_triggers;
    **rules = next_rules;
    drop(rules);
    drop(triggers);
    context.changes.catalog_registry_changed();
    Ok(())
}

pub mod context;
mod lifecycle;

mod dependents;

pub mod persistence;
