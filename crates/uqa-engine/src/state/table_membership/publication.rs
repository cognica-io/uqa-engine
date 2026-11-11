//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table publication owns the exact durable source groups and their child update addresses.

use super::{Membership, TableSources, Tables};
use crate::TableState;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, RelationIdentity};
use uqa_storage::{
    memory_capture::{MemoryCaptureMember, MemoryCaptureMembership, MemoryCaptureRegistry},
    StorageBackendResult,
};

pub(super) struct Publication {
    registry: MemoryCaptureRegistry,
    memory: MemoryBudget,
    owner: MemoryCaptureMembership,
    members: BTreeMap<RelationIdentity, MemoryCaptureMember>,
}

#[cfg(test)]
mod tests;

impl Publication {
    pub(super) fn new(
        registry: &MemoryCaptureRegistry,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            registry: registry.clone(),
            memory: memory.clone(),
            owner: MemoryCaptureMembership::new(registry, memory)?,
            members: BTreeMap::new(),
        })
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn registry(&self) -> &MemoryCaptureRegistry {
        &self.registry
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn selected(&self) -> uqa_storage::memory_capture::MemoryCaptureSources {
        self.owner.snapshot()
    }

    pub(super) fn insert(
        &mut self,
        name: &RelationIdentity,
        table: &TableState,
        source: Option<&TableSources>,
    ) -> StorageBackendResult<()> {
        let Some(source) = source else {
            return self.remove(name);
        };
        let mut vectors = table.vector_indexes.write();
        let publication = vectors.source_publication()?;
        let mut values = table.value_indexes.write();
        let value_publication = values.source_publication()?;
        let sources = source
            .select(&self.registry.sources(&self.memory))?
            .with_sources(publication.sources())?
            .with_sources(value_publication.sources())?;
        let member = self.owner.publish(self.members.get(name), &sources)?;
        publication.bind(member.clone());
        value_publication.bind(member.clone());
        self.members.insert(name.clone(), member);
        Ok(())
    }

    pub(super) fn remove(&mut self, name: &RelationIdentity) -> StorageBackendResult<()> {
        if let Some(member) = self.members.get(name) {
            self.owner.remove(member)?;
            self.members.remove(name);
        }
        Ok(())
    }

    pub(super) fn rename(&mut self, from: &RelationIdentity, to: &RelationIdentity) {
        if let Some(member) = self.members.remove(from) {
            self.members.insert(to.clone(), member);
        }
    }

    /// Prepare the complete replacement before rebinding any child. The invoking registry owner holds its publication boundary until the resulting roots and live handles are installed.
    pub(super) fn replacement(
        &self,
        membership: &Membership,
        live: &Tables,
    ) -> StorageBackendResult<Self> {
        let mut next = Self::new(&self.registry, &self.memory)?;
        let empty = self.registry.sources(&self.memory);
        // Admit the disjoint table groups first. Aliased table owners fail before acquiring their vector locks twice.
        for (name, source) in &membership.sources {
            let member = next.owner.publish(None, &source.select(&empty)?)?;
            next.members.insert(name.clone(), member);
        }
        let mut guards: Vec<_> = membership
            .sources
            .iter()
            .map(|(name, _)| {
                (
                    name,
                    live[name].vector_indexes.write(),
                    live[name].value_indexes.write(),
                )
            })
            .collect();
        let mut bindings = Vec::with_capacity(guards.len());
        for (name, vectors, values) in &mut guards {
            let publication = vectors.source_publication()?;
            let value_publication = values.source_publication()?;
            let member = next.members[*name].clone();
            member.replace_sources(&empty, publication.sources())?;
            member.replace_sources(&empty, value_publication.sources())?;
            bindings.push((publication, value_publication, member));
        }
        for (publication, value_publication, member) in bindings {
            publication.bind(member.clone());
            value_publication.bind(member);
        }
        Ok(next)
    }
}
