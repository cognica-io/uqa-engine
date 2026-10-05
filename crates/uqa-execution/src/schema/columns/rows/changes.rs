//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index surviving transaction changes after a rewrite's original rows were retained.

use crate::row_locks::{publication::TransactionRowChange, PendingRowChangeKind};
use uqa_core::{memory::MemoryBudget, CancellationToken, DocId};
use uqa_sql::SQLError;
use uqa_storage::{
    mvcc::{PrivateRecordChanges, PrivateRecordSnapshot, RecordWrite, VersionError},
    read_control::StorageReadControl,
};

/// Borrow the current transaction's surviving row changes in outer-to-inner frame order and append order within each frame. The slices and their order must remain stable for one visit; committing an inner frame appends its changes to its parent, while rollback removes them.
pub trait RewriteRowChanges {
    fn storage_generation(&self, table: &str) -> Result<[u8; 16], SQLError>;
    fn visit_changes(
        &self,
        visit: &mut dyn FnMut(&[TransactionRowChange]) -> Result<(), SQLError>,
    ) -> Result<(), SQLError>;
}

/// Retain the length of the transaction's current prefix. The enclosing statement must keep that prefix alive until all relation rewrites finish.
pub fn marker(owner: &dyn RewriteRowChanges) -> Result<usize, SQLError> {
    let mut length = 0_usize;
    owner.visit_changes(&mut |changes| {
        length = length
            .checked_add(changes.len())
            .ok_or_else(|| SQLError::Internal("column rewrite change marker overflowed".into()))?;
        Ok(())
    })?;
    Ok(length)
}

/// The final presence of each changed identity, including deleted rows. Its immutable private root and spill readers share the rewrite's allowance and cancellation signal.
pub struct ChangedRows {
    snapshot: Option<PrivateRecordSnapshot>,
    control: StorageReadControl,
}

/// Index only changes still present after `marker`. A rewrite removes its source identity and adds its successor in their respective storage generations; repeated changes retain only the final presence. No row payload or complete change log is copied.
pub fn capture_since(
    owner: &dyn RewriteRowChanges,
    marker: usize,
    generation: [u8; 16],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ChangedRows, SQLError> {
    cancellation.check()?;
    let control = StorageReadControl::new(memory, cancellation);
    let records = PrivateRecordChanges::new(memory);
    let mut skip = marker;
    owner.visit_changes(&mut |changes| {
        cancellation.check()?;
        let skipped = skip.min(changes.len());
        skip -= skipped;
        for change in &changes[skipped..] {
            cancellation.check()?;
            note_change(&records, change, generation, &control)?;
        }
        Ok(())
    })?;
    if skip != 0 {
        return Err(SQLError::Internal(
            "column rewrite change marker no longer belongs to this transaction".into(),
        ));
    }
    cancellation.check()?;
    let snapshot = records
        .has_written()
        .then(|| records.snapshot().map_err(storage_error))
        .transpose()?;
    Ok(ChangedRows { snapshot, control })
}

fn note_change(
    records: &PrivateRecordChanges,
    change: &TransactionRowChange,
    generation: [u8; 16],
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    if change.source_generation == generation {
        note(
            records,
            change.pending.key.doc_id,
            matches!(
                change.pending.kind,
                PendingRowChangeKind::Insert | PendingRowChangeKind::Update
            ),
            control,
        )?;
    }
    if let PendingRowChangeKind::Rewrite(successor) = change.pending.kind {
        if change.successor_generation == Some(generation) {
            note(records, successor.doc_id, true, control)?;
        }
    }
    Ok(())
}

fn note(
    records: &PrivateRecordChanges,
    id: DocId,
    present: bool,
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    records
        .apply(
            &[RecordWrite {
                key: &id.to_be_bytes(),
                expected: None,
                value: Some(&[u8::from(present)]),
            }],
            control,
        )
        .map_err(storage_error)
}

impl ChangedRows {
    /// Whether no change of the selected generation was recorded. A final deletion remains a changed identity.
    pub fn is_empty(&self) -> bool {
        self.snapshot.is_none()
    }

    pub fn get(&self, id: DocId) -> Result<Option<bool>, SQLError> {
        self.control.cancellation().check()?;
        let Some(snapshot) = &self.snapshot else {
            return Ok(None);
        };
        snapshot
            .get(&id.to_be_bytes(), &self.control)
            .map_err(storage_error)?
            .map(|write| presence(write.value()))
            .transpose()
    }

    /// Visit every changed identity once in increasing numeric order, including tombstones. Callback errors stop replay immediately and release its readers.
    pub fn visit(
        &self,
        visit: &mut dyn FnMut(DocId, bool) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.control.cancellation().check()?;
        let Some(snapshot) = &self.snapshot else {
            return Ok(());
        };
        let mut cursor = snapshot
            .cursor(&[], None, &self.control)
            .map_err(storage_error)?;
        while let Some(entry) = cursor.next(&self.control).map_err(storage_error)? {
            let id = DocId::from_be_bytes(entry.key().try_into().map_err(|_| {
                SQLError::Internal("a changed rewrite row has an invalid identity".into())
            })?);
            let present = {
                let write = entry.read(&self.control).map_err(storage_error)?;
                presence(write.value())?
            };
            drop(entry);
            visit(id, present)?;
        }
        Ok(())
    }
}

fn presence(value: Option<&[u8]>) -> Result<bool, SQLError> {
    match value {
        Some([0]) => Ok(false),
        Some([1]) => Ok(true),
        _ => Err(SQLError::Internal(
            "a changed rewrite row has an invalid presence".into(),
        )),
    }
}

fn storage_error(error: VersionError) -> SQLError {
    crate::storage_errors::storage_error(
        "retain changed column rewrite rows",
        &error.into_storage_error(),
    )
}

#[cfg(test)]
mod tests;
