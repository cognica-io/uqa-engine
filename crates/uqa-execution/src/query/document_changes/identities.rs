//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The rows a transaction changed, indexed by relation generation and identity, for reads at a fixed snapshot. Each entry records whether its row is present now and whether it was present before the transaction first changed it, so that a read knows its visible rows and its row count without reading the rows. The entries are private records that spill to encrypted temporary files as a transaction's own changes do, and savepoints restore them without copying.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::mvcc::{PrivateRecordChanges, PrivateRecordSnapshot, RecordWrite, VersionError};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{StorageBackendError, StorageBackendResult, StorageSavepointId};

/// The storage generation of a relation.
pub type RelationGeneration = [u8; 16];

const KEY_BYTES: usize = 16 + size_of::<u64>();
/// The entries a page of a scan keeps.
const PAGE_ENTRIES: usize = 1024;

/// The changed rows of one relation generation, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangedRowCounts {
    /// The rows the transaction changed.
    pub changed: u64,
    /// The changed rows that were present before the transaction first changed them.
    pub before: u64,
    /// The changed rows that are present now.
    pub present: u64,
}

impl ChangedRowCounts {
    /// The rows a read of the relation sees, given the rows its snapshot holds.
    pub fn visible_rows(self, snapshot_rows: u64) -> Option<u64> {
        snapshot_rows
            .checked_sub(self.before)?
            .checked_add(self.present)
    }
}

fn key(generation: &RelationGeneration, id: DocId) -> [u8; KEY_BYTES] {
    let mut key = [0; KEY_BYTES];
    key[..16].copy_from_slice(generation);
    key[16..].copy_from_slice(&id.to_be_bytes());
    key
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

/// The presence an entry records: now, and before the transaction first changed the row.
fn decode_entry(value: Option<&[u8]>) -> StorageBackendResult<(bool, bool)> {
    match value {
        Some([present, before]) => Ok((*present == 1, *before == 1)),
        _ => Err(invalid("a changed row entry is malformed")),
    }
}

/// The rows a transaction changed, which reads at a fixed snapshot take as views.
pub struct ChangedIdentities {
    records: PrivateRecordChanges,
    counts: BTreeMap<RelationGeneration, ChangedRowCounts>,
    /// The counts at each savepoint, innermost last.
    savepoints: Vec<(
        StorageSavepointId,
        BTreeMap<RelationGeneration, ChangedRowCounts>,
    )>,
    view: Arc<PrivateRecordSnapshot>,
    current: bool,
}

impl ChangedIdentities {
    pub fn new(control: &StorageReadControl) -> StorageBackendResult<Self> {
        let records = PrivateRecordChanges::new(control.memory());
        let view = Arc::new(
            records
                .snapshot()
                .map_err(VersionError::into_storage_error)?,
        );
        Ok(Self {
            records,
            counts: BTreeMap::new(),
            savepoints: Vec::new(),
            view,
            current: true,
        })
    }

    /// Record that the row `id` of relation generation `generation` is present now or not. `before` is whether the row was present before this change, which only a first change of the row records.
    pub fn note(
        &mut self,
        generation: &RelationGeneration,
        id: DocId,
        before: bool,
        present: bool,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let key = key(generation, id);
        let previous = self
            .snapshot()?
            .get(&key, control)
            .map_err(VersionError::into_storage_error)?
            .map(|write| decode_entry(write.value()))
            .transpose()?;
        let before = previous.map_or(before, |(_, before)| before);
        let value = [u8::from(present), u8::from(before)];
        self.records
            .apply(
                &[RecordWrite {
                    key: &key,
                    expected: None,
                    value: Some(&value),
                }],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        self.current = false;
        let counts = self.counts.entry(*generation).or_default();
        match previous {
            None => {
                counts.changed += 1;
                counts.before += u64::from(before);
                counts.present += u64::from(present);
            }
            Some((was_present, _)) => {
                counts.present = counts.present + u64::from(present) - u64::from(was_present);
            }
        }
        Ok(())
    }

    /// The view of the records written so far, refreshed after writes.
    fn snapshot(&mut self) -> StorageBackendResult<&Arc<PrivateRecordSnapshot>> {
        if !self.current {
            self.view = Arc::new(
                self.records
                    .snapshot()
                    .map_err(VersionError::into_storage_error)?,
            );
            self.current = true;
        }
        Ok(&self.view)
    }

    pub fn savepoint(&mut self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.records
            .savepoint(id)
            .map_err(VersionError::into_storage_error)?;
        self.savepoints.push((id, self.counts.clone()));
        Ok(())
    }

    /// Release the savepoint `id` and those after it, keeping every change. Returns false when this index does not hold the savepoint.
    pub fn release(&mut self, id: StorageSavepointId) -> StorageBackendResult<bool> {
        let Some(position) = self.savepoints.iter().rposition(|(saved, _)| *saved == id) else {
            return Ok(false);
        };
        self.records
            .release_savepoint(id)
            .map_err(VersionError::into_storage_error)?;
        self.savepoints.truncate(position);
        Ok(true)
    }

    /// Return to the savepoint `id`, keeping it for another rollback. Returns false when this index does not hold the savepoint.
    pub fn rollback_to(&mut self, id: StorageSavepointId) -> StorageBackendResult<bool> {
        let Some(position) = self.savepoints.iter().rposition(|(saved, _)| *saved == id) else {
            return Ok(false);
        };
        self.records
            .rollback_to_savepoint(id)
            .map_err(VersionError::into_storage_error)?;
        self.counts.clone_from(&self.savepoints[position].1);
        self.savepoints.truncate(position + 1);
        self.current = false;
        Ok(true)
    }

    /// The counted changes of relation generation `generation`.
    pub fn counts(&self, generation: &RelationGeneration) -> ChangedRowCounts {
        self.counts.get(generation).copied().unwrap_or_default()
    }

    /// An immutable view of the changes of relation generation `generation`, which later changes do not alter; `None` when the transaction changed no row of it.
    pub fn view(
        &mut self,
        generation: &RelationGeneration,
    ) -> StorageBackendResult<Option<ChangedIdentitiesView>> {
        let counts = self.counts(generation);
        if counts.changed == 0 {
            return Ok(None);
        }
        Ok(Some(ChangedIdentitiesView {
            snapshot: Arc::clone(self.snapshot()?),
            generation: *generation,
            counts,
        }))
    }
}

/// An immutable view of the rows a transaction changed in one relation generation.
#[derive(Clone)]
pub struct ChangedIdentitiesView {
    snapshot: Arc<PrivateRecordSnapshot>,
    generation: RelationGeneration,
    counts: ChangedRowCounts,
}

impl ChangedIdentitiesView {
    pub fn counts(&self) -> ChangedRowCounts {
        self.counts
    }

    /// Whether the transaction changed the row `id`, and whether it is present now.
    pub(crate) fn presence(
        &self,
        id: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<bool>> {
        self.snapshot
            .get(&key(&self.generation, id), control)
            .map_err(VersionError::into_storage_error)?
            .map(|write| decode_entry(write.value()).map(|(present, _)| present))
            .transpose()
    }

    /// The changed rows after identity `after`, in identity order.
    pub(crate) fn cursor(&self, after: Option<DocId>) -> IdentityCursor {
        IdentityCursor {
            view: self.clone(),
            page: None,
            position: 0,
            resume: after,
            done: false,
        }
    }

    /// A page of the changed rows after `after`, and whether rows may follow it.
    fn page(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(BudgetedVec<(DocId, bool)>, bool)> {
        let after = after.map(|id| key(&self.generation, id));
        let mut rows = BudgetedVec::new(control.memory());
        let mut full = false;
        self.snapshot
            .visit(
                &self.generation,
                after.as_ref().map(<[u8; KEY_BYTES]>::as_slice),
                control,
                &mut |write| {
                    let id = write.key()[16..]
                        .try_into()
                        .map(DocId::from_be_bytes)
                        .map_err(|_| {
                            VersionError::from(invalid("a changed row key is malformed"))
                        })?;
                    let (present, _) = decode_entry(write.value()).map_err(VersionError::from)?;
                    rows.push((id, present))?;
                    full = rows.len() == PAGE_ENTRIES;
                    Ok(!full)
                },
            )
            .map_err(VersionError::into_storage_error)?;
        Ok((rows, full))
    }
}

/// The changed rows of a view in identity order, each with whether it is present now. The cursor owns its view.
pub(crate) struct IdentityCursor {
    view: ChangedIdentitiesView,
    page: Option<BudgetedVec<(DocId, bool)>>,
    position: usize,
    resume: Option<DocId>,
    done: bool,
}

impl IdentityCursor {
    pub(crate) fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<(DocId, bool)>> {
        loop {
            control.check()?;
            if let Some(entry) = self.page.as_ref().and_then(|page| page.get(self.position)) {
                self.position += 1;
                self.resume = Some(entry.0);
                return Ok(Some(*entry));
            }
            if self.done {
                return Ok(None);
            }
            self.page = None;
            let (page, full) = self.view.page(self.resume, control)?;
            self.done = !full;
            self.page = Some(page);
            self.position = 0;
        }
    }
}

#[cfg(test)]
mod tests;
