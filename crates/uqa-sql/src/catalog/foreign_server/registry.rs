//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable server definitions retain bound identities and exact ordered option ownership.

use super::ForeignServerDefinition;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedNamedDefinitions, NamedDefinitions, StoredDefinition,
    },
    plan::copy::capture_definition_with_control,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type ForeignServers = NamedDefinitions<ForeignServerDefinition>;
pub type CapturedForeignServers = CapturedNamedDefinitions<ForeignServerDefinition>;

impl sealed::Definition for ForeignServerDefinition {}
impl StoredDefinition for ForeignServerDefinition {
    fn is_temporary(&self, _: &str) -> bool {
        false
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        capture_definition_with_control(self, control)
    }
}

#[cfg(test)]
mod tests;
