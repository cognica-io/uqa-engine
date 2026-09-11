//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! State, transaction, and publication inputs for trigger and rule lifecycle execution.
use super::EventCatalogContext;
use crate::{
    catalog::context::CatalogContext,
    schema::{namespaces::SchemaStatementWriter, view_creation::context::ViewCreationTransactions},
};
use uqa_sql::catalog::{
    constraints::ConstraintIdentity, events::definition::lookup::EventLookupContext,
};

pub trait ConstraintTriggerEvents {
    fn forget(&self, identity: &ConstraintIdentity);
    fn rename_trigger(&self, identity: &ConstraintIdentity, name: &str);
    fn rename_constraint(&self, identity: &ConstraintIdentity, name: &str);
}
#[derive(Clone, Copy)]
pub struct EventLifecycleContext<'a> {
    pub lookup: EventLookupContext<'a>,
    pub catalog: EventCatalogContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
    pub views: &'a dyn ViewCreationTransactions,
    pub projection: CatalogContext<'a>,
    pub pending: &'a dyn ConstraintTriggerEvents,
}
