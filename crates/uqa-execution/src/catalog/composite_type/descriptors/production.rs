//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CompositeDescriptorCache, CompositeRegistry};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;
use uqa_sql::{expr::composites::OwnedCompositeTypeDescriptor, SQLError};

impl CompositeDescriptorCache {
    /// Existing immutable metadata keeps its catalog owner. A controlled miss produces only the selected definition and leaves it request-owned, so unrelated types do not consume its allowance or enter a global cache.
    pub fn descriptor_with_control(
        &self,
        registry: &Arc<CompositeRegistry>,
        type_oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
        control.check()?;
        let cached = {
            let entry = self.entry.lock();
            entry
                .as_ref()
                .filter(|indexed| Arc::ptr_eq(&indexed.registry, registry))
                .map(|indexed| indexed.types.get(&type_oid).cloned())
        };
        if let Some(cached) = cached {
            return Ok(cached.map(OwnedCompositeTypeDescriptor::retained));
        }
        for definition in registry.values() {
            control.check()?;
            if definition.oid == type_oid {
                return OwnedCompositeTypeDescriptor::from_produced(
                    definition.descriptor_with_control(control)?,
                    control,
                )
                .map(Some);
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
