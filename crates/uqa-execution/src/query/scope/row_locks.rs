//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple-lock recheck state and scoped identity emission.

use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

use super::{
    recheck_storage_names_match, CteScope, RecheckDoc, RecheckSourceRow, RowLockRecheckPins,
};

type RecheckStoragePin = (String, String, Arc<Vec<RecheckDoc>>);
type SourceRowLocks = Arc<Produced<Vec<ResolvedRowLock>>>;
type RecheckStoragePins = Arc<Produced<Vec<RecheckStoragePin>>>;

#[derive(Clone, Copy, Default)]
pub struct LockIdentityOptions {
    pub emit: bool,
    pub retain_after_lock: bool,
}

#[derive(Clone, Default)]
pub struct RowLockScopeState {
    source_row_locks: Option<SourceRowLocks>,
    recheck: Option<Arc<RowLockRecheckPins>>,
    outer_row: Option<Arc<crate::OwnedPhysicalRow>>,
    storage_pins: Option<RecheckStoragePins>,
}

impl<S: Clone> CteScope<S> {
    fn row_lock_state(&self) -> Option<&RowLockScopeState> {
        self.row_lock.as_ref()
    }

    fn row_lock_state_mut(&mut self) -> &mut RowLockScopeState {
        self.row_lock.get_or_insert_with(RowLockScopeState::default)
    }

    /// Enter one tuple-local recheck execution: base scans of pinned lock targets emit only the candidate's tuples and every nested `LockRows` is suppressed while lock identities keep flowing.
    pub fn activate_row_lock_recheck(&mut self, pins: Arc<RowLockRecheckPins>) {
        self.row_lock_state_mut().recheck = Some(pins);
    }

    pub fn row_lock_recheck_active(&self) -> bool {
        self.row_lock_state()
            .is_some_and(|state| state.recheck.is_some())
    }

    /// Preserve the complete correlated outer row for a tuple-local locking recheck. The rebuilt inner query must receive the same scope overlay as its original lateral execution or its correlation predicate would see NULL after a lock wait and incorrectly discard the refreshed tuple.
    pub fn set_row_lock_outer_row(&mut self, row: crate::OwnedPhysicalRow) {
        self.row_lock_state_mut().outer_row = Some(Arc::new(row));
    }

    pub fn row_lock_outer_row(&self) -> Option<&crate::OwnedPhysicalRow> {
        self.row_lock_state()?.outer_row.as_deref()
    }

    pub(crate) fn outer_row_binding(&self) -> Option<Arc<crate::OwnedPhysicalRow>> {
        self.row_lock_state()
            .and_then(|state| state.outer_row.clone())
    }

    pub(super) fn restore_outer_row_binding(&mut self, row: Option<Arc<crate::OwnedPhysicalRow>>) {
        if row.is_some() || self.row_lock.is_some() {
            self.row_lock_state_mut().outer_row = row;
        }
    }

    pub fn clear_row_lock_outer_row(&mut self) {
        if let Some(state) = self.row_lock.as_mut() {
            state.outer_row = None;
        }
    }

    /// Pinned tuples one base scan must emit during an active recheck.
    pub fn recheck_docs_for_scan(
        &self,
        qualifier: &str,
        storage_name: &str,
    ) -> Option<Arc<Vec<RecheckDoc>>> {
        let state = self.row_lock_state()?;
        if let Some(pins) = state.recheck.as_ref() {
            if let Some(docs) = pins.docs_for_scan(qualifier, storage_name) {
                return Some(docs);
            }
        }
        state
            .storage_pins
            .iter()
            .flat_map(|pins| pins.iter())
            .find(|(pinned_storage, pinned_scan, _)| {
                pinned_scan == qualifier
                    && recheck_storage_names_match(pinned_storage, storage_name)
            })
            .map(|(_, _, docs)| Arc::clone(docs))
    }

    /// Exact copy-row mark for one top-level FROM leaf in the active tuple recheck. Paths use 0/1 for left/right join descent and are scoped to the original `LockRows` source, so nested query aliases cannot collide.
    pub fn recheck_source_row(&self, path: &[u8]) -> Option<RecheckSourceRow> {
        self.row_lock_state()?
            .recheck
            .as_ref()
            .and_then(|pins| pins.source_row(path))
    }

    /// Enter the build of one identity-source lock target's subtree so every base scan of its storage inside the derived table or view is pinned. Pins already active from an enclosing target stay active: the target's base scans may sit below further derived-table boundaries.
    pub fn enter_recheck_storage_pins(&mut self, qualifier: &str) -> RecheckStoragePinScope<'_, S> {
        let added = self
            .row_lock_state()
            .and_then(|state| state.recheck.as_ref())
            .map(|pins| pins.storage_pins_for_identity_source(qualifier))
            .unwrap_or_default();
        let previous = if added.is_empty() {
            PreviousBinding::Unchanged
        } else {
            let state = self.row_lock_state_mut();
            let previous = state.storage_pins.clone();
            let entries: Vec<_> = state
                .storage_pins
                .iter()
                .flat_map(|pins| pins.iter())
                .cloned()
                .chain(added)
                .collect();
            state.storage_pins = Some(
                ProductionControl::uncontrolled()
                    .finish(entries, None)
                    .expect("ordinary recheck pins")
                    .into_shared(&ProductionControl::uncontrolled())
                    .expect("ordinary recheck pin owner"),
            );
            PreviousBinding::Restore(previous)
        };
        RecheckStoragePinScope {
            ctes: self,
            previous,
        }
    }

    pub fn enter_lock_identity_emission(
        &mut self,
        enabled: bool,
    ) -> LockIdentityEmissionScope<'_, S> {
        self.enter_lock_identity_options(LockIdentityOptions {
            emit: enabled,
            retain_after_lock: false,
        })
    }

    pub(crate) fn enter_lock_identity_options(
        &mut self,
        options: LockIdentityOptions,
    ) -> LockIdentityEmissionScope<'_, S> {
        let previous = std::mem::replace(&mut self.lock_identities, options);
        LockIdentityEmissionScope {
            ctes: self,
            previous,
        }
    }

    pub fn enter_source_row_locks(
        &mut self,
        locks: Vec<ResolvedRowLock>,
    ) -> SourceRowLockScope<'_, S> {
        let existing_is_empty = self.row_lock_state().is_none_or(|state| {
            state
                .source_row_locks
                .as_ref()
                .is_none_or(|locks| locks.is_empty())
        });
        let previous = if locks.is_empty() && existing_is_empty {
            PreviousBinding::Unchanged
        } else {
            PreviousBinding::Restore(std::mem::replace(
                &mut self.row_lock_state_mut().source_row_locks,
                if locks.is_empty() {
                    None
                } else {
                    Some(
                        ProductionControl::uncontrolled()
                            .finish(locks, None)
                            .expect("ordinary source row locks")
                            .into_shared(&ProductionControl::uncontrolled())
                            .expect("ordinary source row lock owner"),
                    )
                },
            ))
        };
        SourceRowLockScope {
            ctes: self,
            previous,
        }
    }

    pub fn source_row_lock_for_view(
        &self,
        qualifier: &str,
        storage_name: &str,
    ) -> Option<ResolvedRowLock> {
        self.row_lock_state()?
            .source_row_locks
            .iter()
            .flat_map(|locks| locks.iter())
            .find(|target| {
                target.identity_source
                    && target.qualifier == qualifier
                    && target.storage_name == storage_name
            })
            .cloned()
    }
}

pub struct LockIdentityEmissionScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: LockIdentityOptions,
}

enum PreviousBinding<T> {
    Unchanged,
    Restore(Option<T>),
}

pub struct SourceRowLockScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: PreviousBinding<SourceRowLocks>,
}

pub struct RecheckStoragePinScope<'a, S: Clone> {
    ctes: &'a mut CteScope<S>,
    previous: PreviousBinding<RecheckStoragePins>,
}

impl<S: Clone> std::ops::Deref for RecheckStoragePinScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for RecheckStoragePinScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for RecheckStoragePinScope<'_, S> {
    fn drop(&mut self) {
        if let PreviousBinding::Restore(previous) = &mut self.previous {
            self.ctes.row_lock_state_mut().storage_pins = previous.take();
        }
    }
}

impl<S: Clone> std::ops::Deref for SourceRowLockScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for SourceRowLockScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for SourceRowLockScope<'_, S> {
    fn drop(&mut self) {
        if let PreviousBinding::Restore(previous) = &mut self.previous {
            self.ctes.row_lock_state_mut().source_row_locks = previous.take();
        }
    }
}

impl<S: Clone> std::ops::Deref for LockIdentityEmissionScope<'_, S> {
    type Target = CteScope<S>;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl<S: Clone> std::ops::DerefMut for LockIdentityEmissionScope<'_, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl<S: Clone> Drop for LockIdentityEmissionScope<'_, S> {
    fn drop(&mut self) {
        self.ctes.lock_identities = self.previous;
    }
}

pub use uqa_sql::plan::locking::ResolvedRowLock;
