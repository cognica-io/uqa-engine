//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow live event registries without copying their contents or releasing catalog guards.
use super::{RuleCatalog, TriggerCatalog};
use std::ops::Deref;
pub type RuleCatalogRead<'a> = Box<dyn Deref<Target = RuleCatalog> + 'a>;
pub type TriggerCatalogRead<'a> = Box<dyn Deref<Target = TriggerCatalog> + 'a>;
pub trait EventCatalogReads {
    fn read_rules(&self) -> RuleCatalogRead<'_>;
    fn read_triggers(&self) -> TriggerCatalogRead<'_>;
}
