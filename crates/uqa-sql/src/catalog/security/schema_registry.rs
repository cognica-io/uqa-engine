//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native schema authority uses immutable definitions and excludes temporary namespaces at capture.

use super::BoundSchemaSecurity;
use crate::{
    catalog::named_definitions::{sealed, StoredDefinition},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type SchemaRegistry = crate::catalog::named_definitions::NamedDefinitions<BoundSchemaSecurity>;
pub type CapturedSchemaRegistry =
    crate::catalog::named_definitions::CapturedNamedDefinitions<BoundSchemaSecurity>;

impl sealed::Definition for BoundSchemaSecurity {}
impl StoredDefinition for BoundSchemaSecurity {
    fn is_temporary(&self, name: &str) -> bool {
        crate::catalog::temporary_namespace::is_temporary_schema_name(name)
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        self.capture_security(control)
    }
}

#[cfg(test)]
mod tests;
