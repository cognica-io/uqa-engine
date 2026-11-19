//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native view generations retain stored syntax, authority and evaluated rows together.

use super::StoredView;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionRegistry, StoredDefinition,
    },
    plan::copy::capture_stored_with_control,
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub type ViewDefinitions = DefinitionRegistry<RelationIdentity, StoredView>;
pub type CapturedViewDefinitions = CapturedDefinitions<RelationIdentity, StoredView>;

impl sealed::Definition for StoredView {}
impl StoredDefinition<RelationIdentity> for StoredView {
    fn is_temporary(&self, _: &RelationIdentity) -> bool {
        self.persistence == crate::ast::RelationPersistence::Temporary
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        capture_stored_with_control(self, control)
    }
}

#[cfg(test)]
mod tests;
