//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native immutable names carry independently admitted weak source identities.

use super::{CapturedValueIndex, Entry};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryError, ProductionControl, RetainedMap},
    CancellationToken,
};
use uqa_storage::{
    memory_capture::{
        MemoryCaptureKey, MemoryCaptureMember, MemoryCaptureRegistry, MemoryCaptureSources,
    },
    StorageBackendResult, ValueIndexKey,
};

pub(super) type IndexSource = MemoryCaptureKey<CapturedValueIndex>;
pub(super) type Names = RetainedMap<ValueIndexKey, Budgeted<IndexSource>>;

#[derive(Clone)]
pub(super) struct NativeValueSources {
    pub(super) registry: MemoryCaptureRegistry,
    pub(super) names: Names,
    pub(super) sources: MemoryCaptureSources,
    pub(super) member: Option<MemoryCaptureMember>,
}

impl NativeValueSources {
    pub(super) fn into_mutation_budget(mut self, memory: &MemoryBudget) -> Self {
        self.names = self.names.into_mutation_budget(memory);
        self.sources = self.sources.into_mutation_budget(memory);
        self
    }

    pub(super) fn new(registry: &MemoryCaptureRegistry, memory: &MemoryBudget) -> Self {
        Self {
            registry: registry.clone(),
            names: Names::new(memory),
            sources: registry.sources(memory),
            member: None,
        }
    }

    pub(super) fn with_entry(
        &self,
        name: &ValueIndexKey,
        entry: &Entry,
    ) -> StorageBackendResult<Self> {
        let mut next = self.without_key(name)?;
        let Entry::Captured(cell) = entry else {
            return Ok(next);
        };
        let key = cell.key()?;
        let cancellation = CancellationToken::new();
        let name = ProductionControl::new(self.names.budget(), &cancellation, &cancellation)
            .copy_text(name.name())
            .map_err(super::super::capture::value_error)?
            .into_budgeted()
            .expect("admitted source name");
        let (name, mut memory) = name.into_parts();
        let bytes = key.retained_source_bytes();
        let footprint = name
            .capacity()
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        memory.absorb(self.names.budget().reserve(bytes)?);
        next.sources = next.sources.with_source(&key)?;
        next.names.insert(
            ValueIndexKey::Index(name),
            Budgeted::new(key, memory),
            footprint,
        )?;
        Ok(next)
    }

    pub(super) fn without_key(&self, name: &ValueIndexKey) -> StorageBackendResult<Self> {
        let mut next = self.clone();
        if let Some(previous) = self.names.get(name) {
            next.sources = next.sources.without_source(previous)?;
            next.names.remove(name)?;
        }
        Ok(next)
    }

    pub(super) fn publish_key(
        &self,
        name: &ValueIndexKey,
        next: &Self,
    ) -> StorageBackendResult<()> {
        if let Some(member) = &self.member {
            member.replace_source(
                self.names.get(name).map(|key| &**key),
                next.names.get(name).map(|key| &**key),
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
