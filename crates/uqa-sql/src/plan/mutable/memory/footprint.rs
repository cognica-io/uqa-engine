//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named fields carry their own measured descendants across subtree transfers. Observation borrows existing field slots as return links, restoring every child to its original address.

use std::sync::{PoisonError, RwLock};

use super::{BudgetedVec, MemoryReservation, RewriteField, RewriteMemory};

/// Shared readers observe the same private root. Field edits have exclusive access and subtree transfers carry their existing measurements without an additional lock per descendant.
#[derive(Debug, Default)]
pub(in crate::plan::mutable) struct RootMemory(RwLock<Option<RewriteMemory>>);

impl From<Option<RewriteMemory>> for RootMemory {
    fn from(memory: Option<RewriteMemory>) -> Self {
        Self(RwLock::new(memory))
    }
}

impl RootMemory {
    pub(in crate::plan::mutable) fn read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, Option<RewriteMemory>> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::plan::mutable) fn as_mut(&mut self) -> Option<&mut RewriteMemory> {
        self.0
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
    }

    pub(in crate::plan::mutable) fn into_parts(self) -> Option<RewriteMemory> {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::plan::mutable) fn reserved_bytes(&self) -> usize {
        if let Some(bytes) = self
            .read()
            .as_ref()
            .map_or(Some(0), RewriteMemory::known_bytes)
        {
            return bytes;
        }
        let mut root = self.0.write().unwrap_or_else(PoisonError::into_inner);
        root.as_mut().map_or(0, RewriteMemory::reserved_bytes)
    }
}

/// Completed constructors can reorder or retire entries in place. Growth transfers the buffer and its lease to the budgeted constructor.
#[derive(Debug)]
pub(crate) struct RewriteFields {
    entries: Vec<RewriteField>,
    // A field contributes to this total after observation completes its child. Reordering or truncating the whole buffer requires observing its surviving entries again.
    bytes: usize,
    pending: PendingFields,
}

/// A field borrow invalidates one index, while a buffer borrow invalidates a suffix. An allocated slice always fits in `isize::MAX` bytes, leaving the top index bit available to distinguish these two cases without enlarging every nested constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingFields(usize);

impl PendingFields {
    const SINGLE: usize = 1 << (usize::BITS - 1);
    const ALL: Self = Self(0);

    fn single(index: usize) -> Self {
        Self(Self::SINGLE | index)
    }

    fn index(self) -> usize {
        self.0 & !Self::SINGLE
    }

    fn contains(self, index: usize) -> bool {
        if self.0 & Self::SINGLE != 0 {
            self.index() == index
        } else {
            self.index() <= index
        }
    }

    fn advance(&mut self, len: usize) {
        self.0 = if self.0 & Self::SINGLE != 0 {
            len
        } else {
            self.index() + 1
        };
    }
}

impl std::ops::Deref for RewriteFields {
    type Target = [RewriteField];
    fn deref(&self) -> &[RewriteField] {
        &self.entries
    }
}

impl std::ops::DerefMut for RewriteFields {
    fn deref_mut(&mut self) -> &mut [RewriteField] {
        self.invalidate();
        &mut self.entries
    }
}

impl RewriteFields {
    pub(super) fn new(entries: Vec<RewriteField>) -> Self {
        Self {
            entries,
            bytes: 0,
            pending: PendingFields::ALL,
        }
    }

    pub(crate) fn into_budgeted(self, direct: &mut MemoryReservation) -> BudgetedVec<RewriteField> {
        let buffer = direct.split(self.entries.capacity() * size_of::<RewriteField>());
        BudgetedVec::from_parts(self.entries, buffer)
    }

    pub(in crate::plan::mutable) fn truncate(&mut self, len: usize) {
        self.invalidate();
        self.entries.truncate(len);
    }

    pub(in crate::plan::mutable) fn pop(&mut self) -> Option<RewriteField> {
        self.invalidate();
        self.entries.pop()
    }

    fn next(&self) -> Option<usize> {
        let index = self.pending.index();
        (index < self.entries.len()).then_some(index)
    }

    fn include(&mut self, bytes: usize) {
        self.bytes += bytes;
        self.pending.advance(self.entries.len());
    }

    fn invalidate(&mut self) {
        self.bytes = 0;
        self.pending = PendingFields::ALL;
    }

    pub(super) fn edit_item(&mut self, index: usize) -> &mut RewriteField {
        if !self.pending.contains(index) {
            // Settle the preceding edit before excluding a different child from the cached sum.
            self.child_bytes();
            let old = self.entries[index]
                .memory
                .as_ref()
                .map_or(Some(0), RewriteMemory::known_bytes)
                .expect("measured constructor child");
            self.bytes -= old;
            self.pending = PendingFields::single(index);
        }
        &mut self.entries[index]
    }

    pub(super) fn known_child_bytes(&self) -> Option<usize> {
        self.next().is_none().then_some(self.bytes)
    }

    pub(super) fn child_bytes(&mut self) -> usize {
        while let Some(index) = self.next() {
            let bytes = Observation::new(&mut self.entries[index].memory).measure();
            self.include(bytes);
        }
        self.bytes
    }
}

/// Each pending constructor stores the previous ancestor in its selected field. The same edge exchange serves reversible observation and consuming retirement.
#[derive(Default)]
pub(super) struct FieldPath {
    pub(super) parent: Option<RewriteMemory>,
}

impl FieldPath {
    pub(super) fn descend(&mut self, current: &mut Option<RewriteMemory>, index: usize) {
        self.exchange(current, index);
        std::mem::swap(current, &mut self.parent);
    }

    pub(super) fn ascend(&mut self, current: &mut Option<RewriteMemory>, index: usize) {
        std::mem::swap(current, &mut self.parent);
        self.exchange(current, index);
    }

    fn exchange(&mut self, current: &mut Option<RewriteMemory>, index: usize) {
        let Some(RewriteMemory::Fields { fields, .. }) = current else {
            unreachable!("constructor return link")
        };
        std::mem::swap(&mut fields.entries[index].memory, &mut self.parent);
    }
}

struct Observation<'a> {
    slot: &'a mut Option<RewriteMemory>,
    current: Option<RewriteMemory>,
    path: FieldPath,
}

impl<'a> Observation<'a> {
    fn new(slot: &'a mut Option<RewriteMemory>) -> Self {
        Self {
            current: slot.take(),
            slot,
            path: FieldPath::default(),
        }
    }

    fn measure(mut self) -> usize {
        loop {
            let bytes = match &mut self.current {
                Some(RewriteMemory::Fields { direct, fields }) => {
                    if let Some(index) = fields.next() {
                        self.path.descend(&mut self.current, index);
                        continue;
                    }
                    direct.bytes() + fields.bytes
                }
                leaf => leaf
                    .as_ref()
                    .map_or(Some(0), RewriteMemory::known_bytes)
                    .expect("scalar footprint"),
            };
            if !self.ascend() {
                return bytes;
            }
            let Some(RewriteMemory::Fields { fields, .. }) = &mut self.current else {
                unreachable!("restored constructor");
            };
            fields.include(bytes);
        }
    }

    fn ascend(&mut self) -> bool {
        let Some(RewriteMemory::Fields { fields, .. }) = &mut self.path.parent else {
            return false;
        };
        let index = fields.pending.index();
        self.path.ascend(&mut self.current, index);
        true
    }
}

impl Drop for Observation<'_> {
    fn drop(&mut self) {
        while self.ascend() {}
        *self.slot = self.current.take();
    }
}

#[cfg(test)]
mod tests;
