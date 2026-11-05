//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication owners maintain the exact eligible source set before registering a cut.

use super::MemoryCaptureSources;
use crate::{memory_capture::MemoryCaptureRegistry, StorageBackendError, StorageBackendResult};
use parking_lot::Mutex;
use std::sync::{Arc, Weak};
use uqa_core::memory::{Budgeted, MemoryBudget, OwnedMap};

struct Published {
    sources: MemoryCaptureSources,
    members: OwnedMap<u64, MemoryCaptureSources>,
    next_member: u64,
}

type Owner = Budgeted<Mutex<Published>>;

/// A mutable publication owner for disjoint groups of source incarnations. Each successful update publishes one immutable exact selection. Snapshotting that selection visits neither members nor sources; the enclosing transaction owner must keep selection and cut registration inside its publication boundary.
pub struct MemoryCaptureMembership(Arc<Owner>);

/// An update address for one published incarnation. This weak address retains neither its publication owner nor any mutable source. Removing or replacing its member permanently retires the address; later child updates cannot restore its eligibility.
#[derive(Clone)]
pub struct MemoryCaptureMember {
    owner: Weak<Owner>,
    id: u64,
}

impl MemoryCaptureMembership {
    pub fn new(
        registry: &MemoryCaptureRegistry,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Self> {
        Ok(Self(
            Budgeted::new(
                Mutex::new(Published {
                    sources: registry.sources(memory),
                    members: OwnedMap::new(),
                    next_member: 1,
                }),
                memory.empty_reservation(),
            )
            .into_shared()?,
        ))
    }

    pub fn snapshot(&self) -> MemoryCaptureSources {
        self.0.lock().sources.clone()
    }

    /// Replace one member or publish a new one. The previous selection and update address remain valid if any identity, overlap or allocation check fails. The owning caller publishes its corresponding live handles only after this preparation succeeds.
    pub fn publish(
        &self,
        previous: Option<&MemoryCaptureMember>,
        sources: &MemoryCaptureSources,
    ) -> StorageBackendResult<MemoryCaptureMember> {
        let mut state = self.0.lock();
        check_domain(&state.sources, sources)?;
        let mut next = state.sources.clone();
        if let Some(previous) = previous {
            self.check_member(previous)?;
            let original = state.members.get(&previous.id).ok_or_else(retired)?;
            for (&id, ()) in &original.root {
                next = next.without_identity(id)?;
            }
        }
        for (&id, ()) in &sources.root {
            if next.root.get(&id).is_some() {
                return Err(overlap());
            }
            next = next.with_identity(id)?;
        }
        let id = state.next_member;
        let next_id = id.checked_add(1).ok_or_else(|| {
            StorageBackendError::Other("memory membership identity space exhausted".into())
        })?;
        state
            .members
            .insert_budgeted(id, sources.clone(), self.0.budget())?;
        if let Some(previous) = previous {
            state.members.remove(&previous.id);
        }
        state.sources = next;
        state.next_member = next_id;
        Ok(MemoryCaptureMember {
            owner: Arc::downgrade(&self.0),
            id,
        })
    }

    /// Retire a published incarnation before unpublishing its live handles. Earlier snapshots retain their exact sources; child updates through the retired address have no effect on future cuts.
    pub fn remove(&self, member: &MemoryCaptureMember) -> StorageBackendResult<bool> {
        self.check_member(member)?;
        let mut state = self.0.lock();
        let Some(original) = state.members.get(&member.id) else {
            return Ok(false);
        };
        let mut next = state.sources.clone();
        for (&id, ()) in &original.root {
            next = next.without_identity(id)?;
        }
        state.members.remove(&member.id);
        state.sources = next;
        Ok(true)
    }

    fn check_member(&self, member: &MemoryCaptureMember) -> StorageBackendResult<()> {
        if !Weak::ptr_eq(&member.owner, &Arc::downgrade(&self.0)) {
            return Err(StorageBackendError::Other(
                "member belongs to another memory publication owner".into(),
            ));
        }
        Ok(())
    }
}

impl MemoryCaptureMember {
    /// Change one selected child without traversing its containing member. The temporary identities and changed paths use the publication owner's allowance.
    pub fn replace_source<F: Send + Sync + 'static>(
        &self,
        previous: Option<&crate::memory_capture::MemoryCaptureKey<F>>,
        source: Option<&crate::memory_capture::MemoryCaptureKey<F>>,
    ) -> StorageBackendResult<bool> {
        let Some(owner) = self.owner.upgrade() else {
            return Ok(false);
        };
        let empty = {
            let state = owner.lock();
            if !state.members.contains_key(&self.id) {
                return Ok(false);
            }
            state.sources.empty()
        };
        let previous = previous.map_or_else(|| Ok(empty.clone()), |key| empty.with_source(key))?;
        let source = source.map_or_else(|| Ok(empty.clone()), |key| empty.with_source(key))?;
        self.replace_sources(&previous, &source)
    }

    /// Publish a child's changed identities together with its containing member. Retired members return false without admitting or retaining those identities. A live member rejects foreign domains, missing previous identities or overlap with another child/member before either root changes.
    pub fn replace_sources(
        &self,
        previous: &MemoryCaptureSources,
        sources: &MemoryCaptureSources,
    ) -> StorageBackendResult<bool> {
        let Some(owner) = self.owner.upgrade() else {
            return Ok(false);
        };
        let mut state = owner.lock();
        let Some(original) = state.members.get(&self.id) else {
            return Ok(false);
        };
        check_domain(original, previous)?;
        check_domain(original, sources)?;
        let mut member = original.clone();
        let mut selected = state.sources.clone();
        for (&id, ()) in &previous.root {
            if original.root.get(&id).is_none() {
                return Err(StorageBackendError::Other(
                    "child source is absent from its published member".into(),
                ));
            }
            if sources.root.get(&id).is_none() {
                member = member.without_identity(id)?;
                selected = selected.without_identity(id)?;
            }
        }
        for (&id, ()) in &sources.root {
            if previous.root.get(&id).is_none() {
                if state.sources.root.get(&id).is_some() {
                    return Err(overlap());
                }
                member = member.with_identity(id)?;
                selected = selected.with_identity(id)?;
            }
        }
        *state.members.get_mut(&self.id).expect("admitted member") = member;
        state.sources = selected;
        Ok(true)
    }
}

fn check_domain(
    left: &MemoryCaptureSources,
    right: &MemoryCaptureSources,
) -> StorageBackendResult<()> {
    if !Weak::ptr_eq(&left.registry, &right.registry) {
        return Err(StorageBackendError::Other(
            "sources belong to another memory capture domain".into(),
        ));
    }
    Ok(())
}

fn overlap() -> StorageBackendError {
    StorageBackendError::Other("memory source already belongs to a published member".into())
}

fn retired() -> StorageBackendError {
    StorageBackendError::Other("memory publication member is retired".into())
}

#[cfg(test)]
mod tests;
