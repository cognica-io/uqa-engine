//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable wrapper generations retain original handler identities and ordered options.

use super::ForeignWrapperDefinition;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedNamedDefinitions, NamedDefinitions, StoredDefinition,
    },
    plan::copy::capture_definition_with_control,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type ForeignWrappers = NamedDefinitions<ForeignWrapperDefinition>;
pub type CapturedForeignWrappers = CapturedNamedDefinitions<ForeignWrapperDefinition>;

impl sealed::Definition for ForeignWrapperDefinition {}
impl StoredDefinition for ForeignWrapperDefinition {
    fn is_temporary(&self, _: &str) -> bool {
        false
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        capture_definition_with_control(self, control)
    }
}

#[cfg(test)]
mod tests;
