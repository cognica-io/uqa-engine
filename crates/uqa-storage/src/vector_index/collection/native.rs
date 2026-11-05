//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native vector membership publishes admitted immutable field and source identities together.

use crate::{
    memory_capture::{
        MemoryCaptureKey, MemoryCaptureMember, MemoryCaptureRegistry, MemoryCaptureSources,
    },
    vector_index::{image::VectorImageSnapshot, VectorIndex},
    StorageBackendError, StorageBackendResult,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryError, Produced, ProductionControl, RetainedMap},
    CancellationToken, FieldName,
};

pub(super) type VectorSourceKey = MemoryCaptureKey<VectorImageSnapshot>;
pub(super) type VectorSourceFields = RetainedMap<Produced<FieldName>, Budgeted<VectorSourceKey>>;

#[derive(Clone)]
pub(super) struct NativeVectorSources {
    pub(super) registry: MemoryCaptureRegistry,
    pub(super) fields: VectorSourceFields,
    pub(super) sources: MemoryCaptureSources,
    pub(super) member: Option<MemoryCaptureMember>,
}

impl NativeVectorSources {
    pub(super) fn new(registry: &MemoryCaptureRegistry, memory: &MemoryBudget) -> Self {
        Self {
            registry: registry.clone(),
            fields: RetainedMap::new(memory),
            sources: registry.sources(memory),
            member: None,
        }
    }

    pub(super) fn with_index(
        &self,
        field: &str,
        index: &dyn VectorIndex,
    ) -> StorageBackendResult<Self> {
        let capture = index.memory_capture().ok_or_else(|| {
            StorageBackendError::Other("vector owner lacks memory capture admission".into())
        })?;
        let key = capture.key()?;
        let cancellation = CancellationToken::new();
        let production = ProductionControl::new(self.fields.budget(), &cancellation, &cancellation);
        let mut next = self.clone();
        if let Some(previous) = self.fields.get(field) {
            next.sources = next.sources.without_source(previous)?;
        }
        next.sources = next.sources.with_source(&key)?;
        let field = production.copy_text(field)?;
        let bytes = key.retained_source_bytes();
        let footprint = field
            .capacity()
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let key = Budgeted::new(key, self.fields.budget().reserve(bytes)?);
        next.fields.insert(field, key, footprint)?;
        Ok(next)
    }

    pub(super) fn without_index(&self, field: &str) -> StorageBackendResult<Self> {
        let mut next = self.clone();
        if let Some(previous) = self.fields.get(field) {
            next.sources = next.sources.without_source(previous)?;
        }
        next.fields.remove(field)?;
        Ok(next)
    }

    pub(super) fn publish_field(&self, field: &str, next: &Self) -> StorageBackendResult<()> {
        if let Some(member) = &self.member {
            member.replace_source(
                self.fields.get(field).map(|key| &**key),
                next.fields.get(field).map(|key| &**key),
            )?;
        }
        Ok(())
    }

    pub(super) fn publish_all(&self, next: &mut Self) -> StorageBackendResult<()> {
        if let Some(member) = &self.member {
            member.replace_sources(&self.sources, &next.sources)?;
            next.member = Some(member.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
