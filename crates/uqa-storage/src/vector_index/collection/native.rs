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
    pub(super) fn into_mutation_budget(mut self, memory: &MemoryBudget) -> Self {
        self.fields = self.fields.into_mutation_budget(memory);
        self.sources = self.sources.into_mutation_budget(memory);
        self
    }

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
        let (next, key) = self.prepare_index(field, index)?;
        let cancellation = CancellationToken::new();
        let production = ProductionControl::new(self.fields.budget(), &cancellation, &cancellation);
        let field = production.copy_text(field)?;
        next.finish_index(field, key)
    }

    pub(super) async fn with_index_from(
        &self,
        field: &str,
        index: &dyn VectorIndex,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        input.checkpoint().await?;
        let (next, key) = self.prepare_index(field, index)?;
        let field = input.copy_text(field).await?;
        next.finish_index(field.into(), key)
    }

    fn prepare_index(
        &self,
        field: &str,
        index: &dyn VectorIndex,
    ) -> StorageBackendResult<(Self, VectorSourceKey)> {
        let capture = index.memory_capture().ok_or_else(|| {
            StorageBackendError::Other("vector owner lacks memory capture admission".into())
        })?;
        let key = capture.key()?;
        let mut next = self.clone();
        if let Some(previous) = self.fields.get(field) {
            next.sources = next.sources.without_source(previous)?;
        }
        next.sources = next.sources.with_source(&key)?;
        Ok((next, key))
    }

    fn finish_index(
        mut self,
        field: Produced<FieldName>,
        key: VectorSourceKey,
    ) -> StorageBackendResult<Self> {
        if !field.is_retained_by(self.fields.budget()) {
            return Err(StorageBackendError::Other(
                "vector field belongs to another allowance".into(),
            ));
        }
        let bytes = key.retained_source_bytes();
        let footprint = field
            .capacity()
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let key = Budgeted::new(key, self.fields.budget().reserve(bytes)?);
        self.fields.insert(field, key, footprint)?;
        Ok(self)
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
