//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy immutable capture belongs to each source's mutation boundary.

use std::{
    any::Any,
    ops::{Bound, Deref, DerefMut},
    sync::{atomic::AtomicU64, atomic::Ordering, Arc, Weak},
    time::Duration,
};

use parking_lot::{MappedRwLockWriteGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uqa_core::memory::{Budgeted, MemoryReservation, OwnedMap};

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod failure;
mod selection;
use selection::SelectedSources;
pub use selection::{MemoryCaptureMember, MemoryCaptureMembership, MemoryCaptureSources};
#[cfg(test)]
mod tests;

const WAIT: Duration = Duration::from_millis(10);
type Erased = Arc<dyn Any + Send + Sync>;

/// An owning source can freeze its current value without retaining a mutable alias. The returned value must own its complete retention admission and original read control. This method may not mutate sources or reenter the capture registry. Capture work and allocation belong to this source's existing snapshot contract.
pub trait MemoryCaptureSource: Send + Sync {
    type Frozen: Send + Sync + 'static;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen>;

    /// Sources containing other source identities bind their frozen metadata to this exact cut. The opaque identity grants no access to registry locks or other sources during preservation.
    fn freeze_at(
        &self,
        control: &StorageReadControl,
        _identity: &MemoryCaptureIdentity,
    ) -> StorageBackendResult<Self::Frozen> {
        self.freeze(control)
    }
}

/// An admitted detached value keeps the original capture cancellation after its source or registry is gone. Payload allocation belongs to the producing owner; the capture registry admits this wrapper.
pub struct CapturedMemoryValue<T> {
    value: Budgeted<T>,
    control: StorageReadControl,
}

impl<T> CapturedMemoryValue<T> {
    pub fn new(value: Budgeted<T>, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        if !value.budget().shares_allowance(control.memory()) {
            return Err(StorageBackendError::Other(
                "captured value belongs to another allowance".into(),
            ));
        }
        Ok(Self {
            value,
            control: control.clone(),
        })
    }

    /// The original retention scope accompanies an independently controlled child reader.
    pub fn read_control(&self) -> &StorageReadControl {
        &self.control
    }

    pub fn get(&self) -> StorageBackendResult<&T> {
        self.control.check()?;
        Ok(&self.value)
    }
}

#[derive(Clone)]
pub struct MemoryCaptureIdentity {
    registry: Weak<Registry>,
    id: u64,
}

impl MemoryCaptureIdentity {
    pub fn matches(&self, capture: &MemoryCapture) -> bool {
        Weak::ptr_eq(&self.registry, &capture.0.registry) && self.id == capture.0.id
    }
}

/// A process-local capture domain. Registering a capture does not visit its sources. A source freezes on its first captured read or immediately before its first subsequent mutation. The surrounding transaction owner must register captures at a committed boundary; this registry alone does not make a sequence of separate cell writes a transaction.
#[derive(Clone, Default)]
pub struct MemoryCaptureRegistry(Arc<Registry>);

struct Registry {
    admission: RwLock<()>,
    captures: Mutex<OwnedMap<u64, Weak<Capture>>>,
    next_capture: AtomicU64,
    next_cell: AtomicU64,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            admission: RwLock::new(()),
            captures: Mutex::default(),
            next_capture: AtomicU64::new(1),
            next_cell: AtomicU64::new(1),
        }
    }
}

impl MemoryCaptureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one cut without enumerating cells or copying source data. Admission waits observe the original cancellation. The returned cut is unavailable if any required source later cannot preserve it; such failure does not fail the source's ordinary write.
    pub fn capture(&self, control: &StorageReadControl) -> StorageBackendResult<MemoryCapture> {
        self.register_capture(SelectedSources::All, control)
    }

    /// Register an immutable selection without visiting its sources. Unselected writes and retirement do not preserve payloads for this cut. The transaction owner supplies the matching source generation and publication boundary.
    pub fn capture_selected(
        &self,
        sources: &MemoryCaptureSources,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryCapture> {
        self.register_capture(
            SelectedSources::Exact(sources.admit(self, control)?),
            control,
        )
    }

    /// Register the union of two already published roots without merging or enumerating either set. Both roots retain their original independent admission; overlapping identities are preserved only once by the cut.
    pub fn capture_selected_union(
        &self,
        first: &MemoryCaptureSources,
        second: &MemoryCaptureSources,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryCapture> {
        let first = first.admit(self, control)?;
        let second = second.admit(self, control)?;
        self.register_capture(SelectedSources::Union([first, second]), control)
    }

    fn register_capture(
        &self,
        sources: SelectedSources,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryCapture> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Capture>())?;
        let _admission = loop {
            control.check()?;
            if let Some(guard) = self.0.admission.try_write_for(WAIT) {
                break guard;
            }
        };
        control.check()?;
        let id = allocate(&self.0.next_capture)?;
        let capture = Arc::new(Capture {
            registry: Arc::downgrade(&self.0),
            id,
            control: control.clone(),
            state: Mutex::new(State::Active {
                sources,
                values: OwnedMap::new(),
            }),
            _memory: memory,
        });
        // The registration lock is gone before a failed insertion can drop the capture and retire its registration.
        self.0
            .captures
            .lock()
            .insert_budgeted(id, Arc::downgrade(&capture), control.memory())?;
        capture.check()?;
        Ok(MemoryCapture(capture))
    }

    /// A new cell cannot appear in cuts registered before its creation. All logical mutations of its source must use this cell's write guard; shared reads may not mutate logical state through interior mutability.
    pub fn cell<T: MemoryCaptureSource>(
        &self,
        value: T,
    ) -> StorageBackendResult<MemoryCaptureCell<T>> {
        let mut cell = MemoryCaptureCell::new(value);
        cell.register(self)?;
        Ok(cell)
    }
}

fn allocate(next: &AtomicU64) -> StorageBackendResult<u64> {
    uqa_core::atomic::try_update_u64(next, Ordering::Relaxed, Ordering::Relaxed, |n| {
        n.checked_add(1)
    })
    .map_err(|_| StorageBackendError::Other("memory capture identity space exhausted".into()))
}

/// Clones retain the same cut and its original allowance. Dropping the last handle unregisters it and releases its archive; separately retained frozen values keep their own leases.
#[derive(Clone)]
pub struct MemoryCapture(Arc<Capture>);

impl MemoryCapture {
    /// Read-only access to this cut's original retention and cancellation scope.
    pub fn read_control(&self) -> &StorageReadControl {
        &self.0.control
    }

    /// Observe the first failure, including cancellation, and release the failed cut's retained archive. An invalidated cut can never collect newer values or become usable again.
    pub fn check(&self) -> StorageBackendResult<()> {
        self.0.check()
    }
}

/// A source identity does not retain its mutable owner. Once that owner is changed or dropped, the cut retains its admitted frozen value. Reading through a key uses the same selected-generation boundary as reading through the live cell.
pub struct MemoryCaptureKey<F: Send + Sync + 'static> {
    registry: Weak<Registry>,
    id: u64,
    born: u64,
    source: Weak<dyn CaptureRead<F>>,
    source_bytes: usize,
}

trait CaptureRead<F: Send + Sync + 'static>: Send + Sync {
    fn captured(&self, capture: &MemoryCapture) -> StorageBackendResult<Arc<Budgeted<F>>>;
}

impl<F: Send + Sync + 'static> Clone for MemoryCaptureKey<F> {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            id: self.id,
            born: self.born,
            source: self.source.clone(),
            source_bytes: self.source_bytes,
        }
    }
}

impl<F: Send + Sync + 'static> MemoryCaptureKey<F> {
    /// Fixed source storage pinned by this weak identity, excluding reference-count bookkeeping. A retained membership owner must admit these bytes before publishing the key. The source's mutable payload is not retained.
    pub fn retained_source_bytes(&self) -> usize {
        self.source_bytes
    }

    pub fn same_source(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.registry, &other.registry) && self.id == other.id
    }

    /// Read the exact selected generation without retaining a mutable Engine or table. An unchanged live source freezes through its owning cell; a retired source has already preserved every admitted cut before releasing its last owning reference.
    pub fn captured(&self, capture: &MemoryCapture) -> StorageBackendResult<Arc<Budgeted<F>>> {
        if let Some(source) = self.source.upgrade() {
            return source.captured(capture);
        }
        self.retained(capture)?.ok_or_else(|| {
            capture.0.fail(
                &mut capture.0.state.lock(),
                StorageBackendError::Other("retired memory source has no selected value".into()),
            )
        })
    }

    /// A live unmodified source has not yet frozen and returns `None`. A dropped registered owner preserves its value before releasing it, so its selected cuts either contain that value or report their terminal failure.
    pub fn retained(
        &self,
        capture: &MemoryCapture,
    ) -> StorageBackendResult<Option<Arc<Budgeted<F>>>> {
        if !Weak::ptr_eq(&self.registry, &capture.0.registry) || capture.0.id < self.born {
            return Err(StorageBackendError::Other(
                "source does not belong to this memory capture".into(),
            ));
        }
        capture.0.retained::<F>(self.id)
    }
}

struct Capture {
    registry: Weak<Registry>,
    id: u64,
    control: StorageReadControl,
    state: Mutex<State>,
    _memory: MemoryReservation,
}

enum State {
    Active {
        sources: SelectedSources,
        values: OwnedMap<u64, Erased>,
    },
    Failed(Arc<StorageBackendError>),
}

impl Capture {
    fn retire(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.captures.lock().remove(&self.id);
        }
    }

    fn fail(&self, state: &mut State, error: StorageBackendError) -> StorageBackendError {
        if let State::Failed(first) = state {
            return failure::reproduce(first);
        }
        let error = Arc::new(error);
        *state = State::Failed(error.clone());
        self.retire();
        failure::reproduce(&error)
    }

    fn admit(&self, state: &mut State) -> StorageBackendResult<()> {
        if let State::Failed(error) = state {
            return Err(failure::reproduce(error));
        }
        self.control
            .check()
            .map_err(|error| self.fail(state, error))
    }

    fn check(&self) -> StorageBackendResult<()> {
        self.admit(&mut self.state.lock())
    }

    fn retained<F: Send + Sync + 'static>(
        &self,
        id: u64,
    ) -> StorageBackendResult<Option<Arc<Budgeted<F>>>> {
        let mut state = self.state.lock();
        self.admit(&mut state)?;
        let State::Active { sources, values } = &*state else {
            unreachable!("failed cuts do not admit values")
        };
        if !sources.contains(id) {
            return Err(selection::not_selected());
        }
        Ok(values.get(&id).map(|value| {
            value
                .clone()
                .downcast::<Budgeted<F>>()
                .unwrap_or_else(|_| unreachable!("cell identities retain one frozen type"))
        }))
    }

    fn frozen<T: MemoryCaptureSource>(
        &self,
        id: u64,
        value: &T,
    ) -> StorageBackendResult<Option<Arc<Budgeted<T::Frozen>>>> {
        let mut state = self.state.lock();
        self.admit(&mut state)?;
        let State::Active { sources, values } = &mut *state else {
            unreachable!("failed cuts do not admit values")
        };
        if !sources.contains(id) {
            return Ok(None);
        }
        if !values.contains_key(&id) {
            let result = values.insert_budgeted_with(id, self.control.memory(), || {
                let memory = self
                    .control
                    .memory()
                    .reserve(size_of::<Budgeted<T::Frozen>>())?;
                let frozen = value.freeze_at(
                    &self.control,
                    &MemoryCaptureIdentity {
                        registry: self.registry.clone(),
                        id: self.id,
                    },
                )?;
                self.control.check()?;
                let erased: Erased = Arc::new(Budgeted::new(frozen, memory));
                Ok::<_, StorageBackendError>(erased)
            });
            if let Err(error) = result {
                return Err(self.fail(&mut state, error));
            }
        }
        Ok(Some(
            values[&id]
                .clone()
                .downcast::<Budgeted<T::Frozen>>()
                .unwrap_or_else(|_| unreachable!("cell identities retain one frozen type")),
        ))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Mutable source ownership with automatic preservation before write admission. The source's existing copy-on-write implementation determines detachment cost; writes with no active captures never invoke `freeze`.
pub struct MemoryCaptureCell<T: MemoryCaptureSource> {
    owner: CellOwner<T>,
}

enum CellOwner<T: MemoryCaptureSource> {
    Unregistered(Option<RwLock<T>>),
    Registered(Arc<RegisteredCell<T>>),
}

struct RegisteredCell<T: MemoryCaptureSource> {
    registration: Registration,
    value: RwLock<T>,
}

struct Registration {
    registry: Arc<Registry>,
    id: u64,
    born: u64,
}

impl<T: MemoryCaptureSource> MemoryCaptureCell<T> {
    /// An unregistered owner uses the same read/write boundary without allocating a private capture domain. Register it before publishing it in a shared memory source.
    pub fn new(value: T) -> Self {
        Self {
            owner: CellOwner::Unregistered(Some(RwLock::new(value))),
        }
    }

    /// Publish one stable registered source identity without admitting it to earlier cuts. Re-registering in that domain is harmless; transferring a registered source to another domain is rejected.
    pub fn register(&mut self, registry: &MemoryCaptureRegistry) -> StorageBackendResult<()> {
        if let Some(registration) = self.registration() {
            return if Arc::ptr_eq(&registration.registry, &registry.0) {
                Ok(())
            } else {
                Err(StorageBackendError::Other(
                    "memory capture source is already registered in another domain".into(),
                ))
            };
        }
        let _admission = registry.0.admission.read_recursive();
        let registration = Registration {
            registry: registry.0.clone(),
            id: allocate(&registry.0.next_cell)?,
            born: registry.0.next_capture.load(Ordering::Relaxed),
        };
        let CellOwner::Unregistered(value) = &mut self.owner else {
            unreachable!("registered cells return before assigning another identity")
        };
        let value = value.take().expect("unpublished source value").into_inner();
        self.owner = CellOwner::Registered(Arc::new(RegisteredCell {
            registration,
            value: RwLock::new(value),
        }));
        Ok(())
    }

    fn registration(&self) -> Option<&Registration> {
        match &self.owner {
            CellOwner::Unregistered(_) => None,
            CellOwner::Registered(cell) => Some(&cell.registration),
        }
    }

    fn value(&self) -> &RwLock<T> {
        match &self.owner {
            CellOwner::Unregistered(value) => value.as_ref().expect("unpublished source value"),
            CellOwner::Registered(cell) => &cell.value,
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.value().read()
    }

    /// Observe lock ownership for diagnostics without entering mutation admission.
    pub fn is_locked(&self) -> bool {
        self.value().is_locked()
    }

    /// Retain the registered identity without retaining or copying its source data.
    pub fn key(&self) -> StorageBackendResult<MemoryCaptureKey<T::Frozen>>
    where
        T: 'static,
    {
        let CellOwner::Registered(cell) = &self.owner else {
            return Err(StorageBackendError::Other(
                "memory capture source is not registered".into(),
            ));
        };
        let registration = &cell.registration;
        let source: Arc<dyn CaptureRead<T::Frozen>> = cell.clone();
        Ok(MemoryCaptureKey {
            registry: Arc::downgrade(&registration.registry),
            id: registration.id,
            born: registration.born,
            source: Arc::downgrade(&source),
            source_bytes: size_of::<RegisteredCell<T>>(),
        })
    }

    pub fn read_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RwLockReadGuard<'_, T>> {
        loop {
            control.check()?;
            if let Some(value) = self.value().try_read_for(WAIT) {
                control.check()?;
                return Ok(value);
            }
            #[cfg(test)]
            tests::source_wait();
        }
    }

    /// Preserve every applicable cut before exposing mutable state. A source freeze failure invalidates that cut and releases its archive; the caller can still perform the ordinary write. No caller-side snapshot preparation is needed.
    pub fn write(&self) -> MemoryCaptureWriteGuard<'_, T> {
        self.admit_write(self.value().write())
    }

    /// Try the same mutation boundary without waiting for the source lock. A failed admission neither visits nor freezes any cut.
    pub fn try_write(&self) -> Option<MemoryCaptureWriteGuard<'_, T>> {
        self.value()
            .try_write()
            .map(|value| self.admit_write(value))
    }

    pub fn write_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryCaptureWriteGuard<'_, T>> {
        let value = loop {
            control.check()?;
            if let Some(value) = self.value().try_write_for(WAIT) {
                break value;
            }
            #[cfg(test)]
            tests::source_wait();
        };
        control.check()?;
        let guard = self.admit_write(value);
        control.check()?;
        Ok(guard)
    }

    fn admit_write<'a>(&'a self, value: RwLockWriteGuard<'a, T>) -> MemoryCaptureWriteGuard<'a, T> {
        #[cfg(test)]
        tests::source_locked();
        let admission = self
            .registration()
            .map(|registration| registration.preserve(&*value));
        MemoryCaptureWriteGuard {
            source_guard: parking_lot::RwLockWriteGuard::map(value, |value| value),
            _admission: admission,
        }
    }

    /// Retain this cell's value at the selected cut. Reading an unmodified cell performs its first freeze outside a writer; later reads reuse the same immutable owner. Foreign cuts and cells created later are rejected.
    pub fn captured(
        &self,
        capture: &MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<T::Frozen>>> {
        let CellOwner::Registered(cell) = &self.owner else {
            return Err(StorageBackendError::Other(
                "memory capture source is not registered".into(),
            ));
        };
        cell.captured(capture)
    }
}

impl<T: MemoryCaptureSource> CaptureRead<T::Frozen> for RegisteredCell<T> {
    fn captured(&self, capture: &MemoryCapture) -> StorageBackendResult<Arc<Budgeted<T::Frozen>>> {
        let registration = &self.registration;
        if !Weak::ptr_eq(&capture.0.registry, &Arc::downgrade(&registration.registry))
            || capture.0.id < registration.born
        {
            return Err(StorageBackendError::Other(
                "cell does not belong to this memory capture".into(),
            ));
        }
        let value = loop {
            if let Some(value) = capture.0.retained::<T::Frozen>(registration.id)? {
                return Ok(value);
            }
            if let Some(value) = self.value.try_read_for(WAIT) {
                break value;
            }
            #[cfg(test)]
            tests::source_wait();
        };
        capture
            .0
            .frozen(registration.id, &*value)?
            .ok_or_else(selection::not_selected)
    }
}

impl<T: MemoryCaptureSource> Drop for MemoryCaptureCell<T> {
    fn drop(&mut self) {
        if let CellOwner::Registered(cell) = &self.owner {
            // Preserve before releasing the owning strong reference. A key either upgrades that still-live allocation or observes the completed archive; it cannot miss a source whose final retirement is still preserving its value.
            drop(cell.registration.preserve(&*cell.value.read()));
        }
    }
}

impl Registration {
    fn preserve<T: MemoryCaptureSource>(&self, value: &T) -> RwLockReadGuard<'_, ()> {
        // Recursive read admission permits ordinary nested cell guards even when a capture is waiting. The enclosing transaction boundary supplies multi-cell atomicity.
        let admission = self.registry.admission.read_recursive();
        let mut after = 0;
        loop {
            let next = {
                let captures = self.registry.captures.lock();
                captures
                    .first_from(Bound::Excluded(&after))
                    .map(|(&id, capture)| (id, capture.upgrade()))
            };
            let Some((id, capture)) = next else { break };
            after = id;
            if id >= self.born {
                if let Some(capture) = capture {
                    // The capture owns terminal failure handling; a failed backup cannot consume the source's write operation.
                    drop(capture.frozen(self.id, value));
                }
            }
        }
        admission
    }
}

/// Write admission remains held through the final mutation, including mapped guards.
pub struct MemoryCaptureWriteGuard<'a, T: ?Sized> {
    source_guard: MappedRwLockWriteGuard<'a, T>,
    _admission: Option<RwLockReadGuard<'a, ()>>,
}

impl<'a, T: ?Sized> MemoryCaptureWriteGuard<'a, T> {
    pub fn map<U: ?Sized>(
        guard: Self,
        map: impl FnOnce(&mut T) -> &mut U,
    ) -> MemoryCaptureWriteGuard<'a, U> {
        let Self {
            source_guard,
            _admission: admission,
        } = guard;
        MemoryCaptureWriteGuard {
            source_guard: MappedRwLockWriteGuard::map(source_guard, map),
            _admission: admission,
        }
    }
}

impl<T: ?Sized> Deref for MemoryCaptureWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.source_guard
    }
}

impl<T: ?Sized> DerefMut for MemoryCaptureWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.source_guard
    }
}

impl MemoryCaptureSource for Box<dyn crate::DocumentStore> {
    type Frozen = Arc<dyn crate::DocumentStore>;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        self.snapshot_with_control(control)
    }
}

impl MemoryCaptureSource for Box<dyn crate::InvertedIndex> {
    type Frozen = Arc<dyn crate::InvertedIndex>;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        self.snapshot_with_control(control)
    }
}
