//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each lexical frame borrows its parent and keeps an admitted index of only its own names.

use super::{BindingNameSet, ProductionControl, RelationName, SQLError};
use uqa_core::memory::ProductionMap;

pub(super) trait VisibleNames {
    fn contains(&self, name: RelationName<'_>) -> bool;
}
impl VisibleNames for BindingNameSet {
    fn contains(&self, name: RelationName<'_>) -> bool {
        self.contains_name(name)
    }
}
pub(super) struct LocalNames<'p, 'n, 'c> {
    parent: &'p dyn VisibleNames,
    local: ProductionMap<'c, RelationName<'n>, ()>,
}
impl<'p, 'n, 'c> LocalNames<'p, 'n, 'c> {
    pub(super) fn new(parent: &'p dyn VisibleNames, control: ProductionControl<'c>) -> Self {
        Self {
            parent,
            local: ProductionMap::new(control),
        }
    }
    pub(super) fn insert(&mut self, name: &'n str) -> Result<(), SQLError> {
        self.local.insert_copy(RelationName::decoded(name), ())?;
        Ok(())
    }
}
impl VisibleNames for LocalNames<'_, '_, '_> {
    fn contains(&self, name: RelationName<'_>) -> bool {
        self.local.get(&name).is_some() || self.parent.contains(name)
    }
}

#[cfg(test)]
mod tests;
