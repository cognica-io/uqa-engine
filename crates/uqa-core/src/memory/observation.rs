//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only observations of exposed allocation layouts, independent of admission charges.

use super::{BudgetedMap, BudgetedVec, MemoryBudget, MemoryError, ProductionControl};
use crate::ValueRetentionError;

/// Live value layouts and their exposed allocation capacity. Stack storage and allocator bookkeeping are not part of the observation; each owning interface defines which retained resources belong to its scope.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AllocationUsage {
    pub used_bytes: usize,
    pub allocated_bytes: usize,
}

/// Describe allocations retained in this owner's scope. Inline fields belong to their enclosing allocation; included shared resources must register with `shared_owner` before visiting their payload. Implementations must neither copy nor evaluate the observed value.
pub trait ObserveAllocations {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError>;
}

trait Sequence {
    fn at(&self, index: usize) -> Option<&dyn ObserveAllocations>;
}

enum Pending<'a> {
    Value(&'a dyn ObserveAllocations),
    Sequence(&'a dyn Sequence, usize),
    ValueMap(std::collections::btree_map::Iter<'a, String, crate::Value>),
}

/// An iterative borrowed traversal. Its admitted stack is diagnostic workspace and is never included in the observed owner's totals. Sequences retain one cursor, so workspace follows nesting depth rather than vector width.
pub struct AllocationObserver<'a, 'control> {
    usage: AllocationUsage,
    pending: BudgetedVec<Pending<'a>>,
    shared: BudgetedMap<usize, ()>,
    control: ProductionControl<'control>,
}

impl AllocationUsage {
    pub fn measure(
        owner: &dyn ObserveAllocations,
        control: &ProductionControl<'_>,
    ) -> Result<Self, ValueRetentionError> {
        control.check()?;
        let ordinary;
        let budget = if let Some(budget) = control.budget() {
            budget
        } else {
            ordinary = MemoryBudget::new(usize::MAX);
            &ordinary
        };
        let mut observer = AllocationObserver {
            usage: Self::default(),
            pending: BudgetedVec::new(budget),
            shared: BudgetedMap::new(budget),
            control: *control,
        };
        observer.child(owner)?;
        while let Some(pending) = observer.pending.pop() {
            control.check()?;
            match pending {
                Pending::Value(value) => value.observe_allocations(&mut observer)?,
                Pending::Sequence(values, index) => {
                    if let Some(value) = values.at(index) {
                        observer
                            .pending
                            .push(Pending::Sequence(values, index + 1))?;
                        observer.child(value)?;
                    }
                }
                Pending::ValueMap(mut entries) => {
                    if let Some((key, value)) = entries.next() {
                        observer.pending.push(Pending::ValueMap(entries))?;
                        observer.child(key)?;
                        observer.child(value)?;
                    }
                }
            }
        }
        Ok(observer.usage)
    }
}

impl<'a> AllocationObserver<'a, '_> {
    /// Standard B-tree layouts are opaque; visit exposed document key/value payloads through one borrowed iterator without a width-sized queue.
    pub(crate) fn value_map(
        &mut self,
        values: &'a std::collections::BTreeMap<String, crate::Value>,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        self.pending.push(Pending::ValueMap(values.iter()))?;
        Ok(())
    }

    /// Visit a retained allocation once even when several immutable handles reach it. The borrowed graph keeps every registered identity alive for the complete observation.
    pub fn shared_owner<T: ?Sized>(&mut self, owner: &'a T) -> Result<bool, ValueRetentionError> {
        self.control.check()?;
        let identity = std::ptr::from_ref(owner).cast::<()>() as usize;
        if self.shared.contains_key(&identity) {
            return Ok(false);
        }
        self.shared.insert(identity, ())?;
        Ok(true)
    }

    pub fn child(&mut self, value: &'a dyn ObserveAllocations) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        self.pending.push(Pending::Value(value))?;
        Ok(())
    }

    /// Add the exposed extent of an allocation; this is an observation, never a reservation or a replacement for producer admission.
    pub fn allocation(&mut self, used: usize, capacity: usize) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        assert!(used <= capacity, "initialized allocation exceeds capacity");
        self.usage.used_bytes = self
            .usage
            .used_bytes
            .checked_add(used)
            .ok_or(MemoryError::SizeOverflow)?;
        self.usage.allocated_bytes = self
            .usage
            .allocated_bytes
            .checked_add(capacity)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(())
    }

    pub fn buffer<T>(&mut self, len: usize, capacity: usize) -> Result<(), ValueRetentionError> {
        let bytes = |count: usize| {
            count
                .checked_mul(size_of::<T>())
                .ok_or(MemoryError::SizeOverflow)
        };
        self.allocation(bytes(len)?, bytes(capacity)?)
    }

    pub fn vector<T: ObserveAllocations>(
        &mut self,
        values: &'a Vec<T>,
    ) -> Result<(), ValueRetentionError> {
        self.buffer::<T>(values.len(), values.capacity())?;
        if !values.is_empty() {
            self.pending.push(Pending::Sequence(values, 0))?;
        }
        Ok(())
    }
}

impl ObserveAllocations for String {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        observer.allocation(self.len(), self.capacity())
    }
}

impl<T: ObserveAllocations> Sequence for Vec<T> {
    fn at(&self, index: usize) -> Option<&dyn ObserveAllocations> {
        self.get(index)
            .map(|value| value as &dyn ObserveAllocations)
    }
}

impl<T: ObserveAllocations> ObserveAllocations for Vec<T> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        observer.vector(self)
    }
}

impl<T: ObserveAllocations> ObserveAllocations for Box<T> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        observer.allocation(size_of::<T>(), size_of::<T>())?;
        observer.child(self.as_ref())
    }
}

impl ObserveAllocations for Box<str> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        observer.allocation(self.len(), self.len())
    }
}

impl<T: ObserveAllocations> ObserveAllocations for Option<T> {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        if let Some(value) = self {
            observer.child(value)?;
        }
        Ok(())
    }
}

impl<A: ObserveAllocations, B: ObserveAllocations> ObserveAllocations for (A, B) {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        observer.child(&self.0)?;
        observer.child(&self.1)
    }
}

macro_rules! inline {
    ($($ty:ty),* $(,)?) => {$ (
        impl ObserveAllocations for $ty {
            fn observe_allocations<'a>(&'a self, _: &mut AllocationObserver<'a, '_>) -> Result<(), ValueRetentionError> { Ok(()) }
        }
    )*};
}
inline!(
    (),
    bool,
    u8,
    i8,
    u16,
    i16,
    u32,
    i32,
    u64,
    i64,
    usize,
    isize,
    f32,
    f64
);

#[cfg(test)]
mod tests;
