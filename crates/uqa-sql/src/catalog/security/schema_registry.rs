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
use uqa_core::memory::{Budgeted, Produced, ProductionControl};

pub type SchemaRegistry = crate::catalog::named_definitions::NamedDefinitions<BoundSchemaSecurity>;
pub type CapturedSchemaRegistry =
    crate::catalog::named_definitions::CapturedNamedDefinitions<BoundSchemaSecurity>;

impl BoundSchemaSecurity {
    /// Transfer original names and ACL storage to independent native fields without copying payloads or releasing their admission.
    pub fn from_budgeted_row(
        row: Budgeted<uqa_core::catalog_schema::BoundSchemaRow>,
    ) -> (Budgeted<String>, Budgeted<Self>) {
        let (row, mut memory) = row.into_parts();
        let name_memory = memory.split(row.name.capacity());
        let (name, security) = Self::from_row(row);
        (
            Budgeted::new(name, name_memory),
            Budgeted::new(security, memory),
        )
    }
}

impl SchemaRegistry {
    /// Publish already validated schema fields into the native immutable registry under the same allowance.
    pub fn insert_stored_entry(
        &mut self,
        name: Budgeted<String>,
        security: Budgeted<BoundSchemaSecurity>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.insert_image_entry(name, security)
    }
}

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
