//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable executable roots keep their constructors' reservations through query projection and sharing.

use super::{QueryPlan, UnifiedPlan};
use crate::SQLError;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};

/// An executable and the reservations transferred by its allocation owners. Moving an existing input into this owner performs no allocation and does not retroactively charge that input. Native producers must admit their new allocations before transferring them. The tree is read-only after publication; preparation that constructs additional storage must retain its output reservations too.
#[derive(Debug)]
pub struct OwnedExecutablePlan {
    plan: UnifiedPlan,
    // Drop the complete tree before releasing its constructor reservations.
    memory: ExecutableMemory,
}

/// A relational executable projected from its statement owner. Retaining the original root box avoids copying its payload or releasing a charged box while its reservation is still attached to the query.
#[derive(Debug)]
pub struct OwnedQueryPlan {
    query: Box<QueryPlan>,
    memory: ExecutableMemory,
}

/// Shared immutable execution storage. Keeping the Arc private prevents unwrapping its payload while leaving the shared allocation's reservation attached to an unboxed value.
#[derive(Debug, Clone)]
pub struct SharedExecutablePlan(SharedStorage);

#[derive(Debug, Clone)]
enum SharedStorage {
    Ordinary(Arc<UnifiedPlan>),
    Admitted(Arc<OwnedExecutablePlan>),
}

/// Allocation leases transferred with an executable. The root box is kept separate so projecting an unboxed query releases exactly that allocation, while attached child roots become ordinary tree storage.
#[derive(Debug, Default)]
pub struct ExecutableMemory {
    storage: Option<MemoryReservation>,
    root: Option<MemoryReservation>,
}

impl From<Option<MemoryReservation>> for ExecutableMemory {
    fn from(storage: Option<MemoryReservation>) -> Self {
        Self {
            storage,
            root: None,
        }
    }
}

impl ExecutableMemory {
    pub fn reserved_bytes(&self) -> usize {
        self.storage.as_ref().map_or(0, MemoryReservation::bytes)
            + self.root.as_ref().map_or(0, MemoryReservation::bytes)
    }

    /// A newly attached executable retains its complete allocation ownership in its enclosing tree.
    pub fn absorb(&mut self, child: Self) {
        self.absorb_storage(child.storage);
        self.absorb_storage(child.root);
    }

    fn absorb_storage(&mut self, memory: Option<MemoryReservation>) {
        if let Some(memory) = memory {
            if let Some(storage) = &mut self.storage {
                storage.absorb(memory);
            } else {
                self.storage = Some(memory);
            }
        }
    }
}

impl OwnedExecutablePlan {
    /// Transfer existing storage and the reservations already acquired by its constructors. This is an ownership transfer, not an admission operation.
    pub fn from_parts(plan: UnifiedPlan, memory: impl Into<ExecutableMemory>) -> Self {
        Self {
            plan,
            memory: memory.into(),
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory.reserved_bytes()
    }

    /// Transfer the tree and its leases together to a specialized owning operation. That operation must keep them alive until their corresponding storage is destroyed or transferred; it must admit any new allocation before construction.
    pub fn into_parts(self) -> (UnifiedPlan, ExecutableMemory) {
        (self.plan, self.memory)
    }

    /// Project a query without unboxing, copying or changing any reservation. A command is returned intact to its caller.
    pub fn into_query(self) -> Result<OwnedQueryPlan, Self> {
        let UnifiedPlan::Query(query) = self.plan else {
            return Err(self);
        };
        Ok(OwnedQueryPlan {
            query,
            memory: self.memory,
        })
    }

    /// Admit only the new shared payload; all existing tree allocations retain their original leases. Cloning the returned handle does not allocate or copy the executable.
    pub fn into_shared(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<SharedExecutablePlan, SQLError> {
        for memory in [&self.memory.storage, &self.memory.root]
            .into_iter()
            .flatten()
        {
            assert!(
                control
                    .budget()
                    .is_some_and(|budget| budget.shares_allowance(memory.budget())),
                "executable sharing preserves its constructor allowance"
            );
        }
        let wrapper = control.reserve(size_of::<Self>())?;
        self.memory.absorb_storage(wrapper);
        Ok(SharedExecutablePlan(SharedStorage::Admitted(Arc::new(
            self,
        ))))
    }
}

impl From<UnifiedPlan> for OwnedExecutablePlan {
    fn from(plan: UnifiedPlan) -> Self {
        Self::from_parts(plan, None)
    }
}

impl From<Produced<UnifiedPlan>> for OwnedExecutablePlan {
    fn from(plan: Produced<UnifiedPlan>) -> Self {
        let (plan, mut memory) = plan.into_parts();
        let root_bytes = match &plan {
            UnifiedPlan::Query(_) => size_of::<QueryPlan>(),
            UnifiedPlan::Command(_) => size_of::<super::CommandPlan>(),
        };
        let root = memory.as_mut().map(|memory| memory.split(root_bytes));
        Self::from_parts(
            plan,
            ExecutableMemory {
                storage: memory,
                root,
            },
        )
    }
}

impl AsRef<UnifiedPlan> for OwnedExecutablePlan {
    fn as_ref(&self) -> &UnifiedPlan {
        &self.plan
    }
}

impl std::ops::Deref for OwnedExecutablePlan {
    type Target = UnifiedPlan;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl OwnedQueryPlan {
    /// Admit the enclosing box for an existing query without retroactively charging its payload.
    pub fn from_query(query: QueryPlan, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let root = control.reserve(size_of::<QueryPlan>())?;
        Ok(Self {
            query: Box::new(query),
            memory: ExecutableMemory {
                storage: control.empty_reservation(),
                root,
            },
        })
    }

    /// Preserve an admitted query body and admit its enclosing box before allocating it.
    pub fn from_produced(
        query: Produced<QueryPlan>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let root = control.reserve(size_of::<QueryPlan>())?;
        let (query, memory) = query.into_parts();
        // Validate the original and destination allowance without merging the separately owned box.
        let memory = control.combine(memory, control.empty_reservation());
        Ok(Self {
            query: Box::new(query),
            memory: ExecutableMemory {
                storage: memory,
                root,
            },
        })
    }

    /// Transfer an existing root box together with its constructor reservations.
    pub fn from_parts(query: Box<QueryPlan>, memory: impl Into<ExecutableMemory>) -> Self {
        Self {
            query,
            memory: memory.into(),
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory.reserved_bytes()
    }

    /// Specialized query preparation must keep this pair together and admit its additional source attachments before construction.
    pub fn into_parts(self) -> (Box<QueryPlan>, ExecutableMemory) {
        (self.query, self.memory)
    }

    /// Move the query into an enclosing owner whose representation has no root box. Release that box's own lease only after freeing it; the caller must retain the returned storage leases with the query.
    pub fn into_unboxed(self) -> (QueryPlan, ExecutableMemory) {
        let query = *self.query;
        drop(self.memory.root);
        (
            query,
            ExecutableMemory {
                storage: self.memory.storage,
                root: None,
            },
        )
    }

    /// Transfer the existing root allocation and all of its original leases into an enclosing SQL constructor, without copying or reallocating the box.
    pub(in crate::plan) fn into_produced_box(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Box<QueryPlan>>, SQLError> {
        Ok(control.finish(
            self.query,
            control.combine(self.memory.storage, self.memory.root),
        )?)
    }

    pub fn into_executable(self) -> OwnedExecutablePlan {
        OwnedExecutablePlan::from_parts(UnifiedPlan::Query(self.query), self.memory)
    }
}

impl AsRef<QueryPlan> for OwnedQueryPlan {
    fn as_ref(&self) -> &QueryPlan {
        &self.query
    }
}

impl std::ops::Deref for OwnedQueryPlan {
    type Target = QueryPlan;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl SharedExecutablePlan {
    pub fn strong_count(&self) -> usize {
        match &self.0 {
            SharedStorage::Ordinary(plan) => Arc::strong_count(plan),
            SharedStorage::Admitted(plan) => Arc::strong_count(plan),
        }
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (SharedStorage::Ordinary(left), SharedStorage::Ordinary(right)) => {
                Arc::ptr_eq(left, right)
            }
            (SharedStorage::Admitted(left), SharedStorage::Admitted(right)) => {
                Arc::ptr_eq(left, right)
            }
            _ => false,
        }
    }

    pub fn reserved_bytes(&self) -> usize {
        match &self.0 {
            SharedStorage::Ordinary(_) => 0,
            SharedStorage::Admitted(plan) => plan.reserved_bytes(),
        }
    }
}

impl AsRef<UnifiedPlan> for SharedExecutablePlan {
    fn as_ref(&self) -> &UnifiedPlan {
        match &self.0 {
            SharedStorage::Ordinary(plan) => plan,
            SharedStorage::Admitted(plan) => plan,
        }
    }
}

impl std::ops::Deref for SharedExecutablePlan {
    type Target = UnifiedPlan;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests;

impl From<UnifiedPlan> for SharedExecutablePlan {
    /// Share an ordinary input without claiming admission for allocations its constructors did not reserve.
    fn from(plan: UnifiedPlan) -> Self {
        Self(SharedStorage::Ordinary(Arc::new(plan)))
    }
}

impl From<Arc<UnifiedPlan>> for SharedExecutablePlan {
    fn from(plan: Arc<UnifiedPlan>) -> Self {
        Self(SharedStorage::Ordinary(plan))
    }
}
