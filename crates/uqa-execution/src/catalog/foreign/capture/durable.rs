//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original table membership selects its matching authority before payload admission.

use super::{table, BTreeMap, CapturedForeignTable, RelationIdentity, StoredForeignTable};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    ast::RelationPersistence,
    catalog::security::{table_binding::CapturedTableSecurity, BoundTableSecurity},
    SQLError,
};
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

#[derive(Debug)]
pub struct CapturedForeignRelation {
    pub relation: RelationIdentity,
    pub definition: CapturedForeignTable,
    pub security: CapturedTableSecurity,
}

pub struct ForeignTableCaptureSource {
    pub tables: Arc<BTreeMap<RelationIdentity, StoredForeignTable>>,
    pub security: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
}

impl ForeignTableCaptureSource {
    /// Temporary membership is excluded before authority lookup, output capacity or payload copying.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<CapturedForeignRelation>>, SQLError> {
        let count = self.tables.values().try_fold(0, |count, table| {
            control.check()?;
            Ok::<_, SQLError>(
                count + usize::from(table.persistence != RelationPersistence::Temporary),
            )
        })?;
        let mut output = ProductionVec::new(*control);
        output.reserve(count)?;
        for (relation, definition) in self.tables.as_ref() {
            control.check()?;
            if definition.persistence == RelationPersistence::Temporary {
                continue;
            }
            let security = self.security.get(relation).ok_or_else(|| {
                SQLError::Internal("captured foreign table has no authority metadata".into())
            })?;
            let (schema, schema_memory) = control.copy_text(&relation.schema)?.into_parts();
            let (name, name_memory) = control.copy_text(&relation.name)?.into_parts();
            let (definition, definition_memory) = table(definition, control)?.into_parts();
            let (security, security_memory) = security.capture_security(control)?.into_parts();
            output.push_produced(control.finish(
                CapturedForeignRelation {
                    relation: RelationIdentity { schema, name },
                    definition,
                    security,
                },
                control.combine(
                    control.combine(schema_memory, name_memory),
                    control.combine(definition_memory, security_memory),
                ),
            )?)?;
        }
        Ok(output.finish()?)
    }
}

impl MemoryCaptureSource for ForeignTableCaptureSource {
    type Frozen = CapturedMemoryValue<Vec<CapturedForeignRelation>>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = self
            .capture(&production)
            .map_err(|error| StorageBackendError::backend("foreign catalog capture", error))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("foreign capture uses admitted production"));
        CapturedMemoryValue::new(value, control)
    }
}

#[cfg(test)]
mod tests;
