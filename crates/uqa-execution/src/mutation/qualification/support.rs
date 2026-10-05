//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable ordered document identities with charged resident storage or encrypted positional reads.

use std::{
    cell::RefCell,
    io::{Read, Seek, SeekFrom, Write},
};
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation},
    DocId, ScoredEntry,
};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, temporary_file::BlockTemporaryFile};

const BLOCK: usize = 512;

pub(super) enum DocumentSupport {
    Resident(BudgetedVec<DocId>),
    Spilled {
        file: RefCell<BlockTemporaryFile<BLOCK>>,
        len: u64,
        _workspace: MemoryReservation,
    },
}

fn io(error: std::io::Error) -> SQLError {
    SQLError::Internal(format!("mutation retrieval support: {error}"))
}

pub(super) fn retain_support(
    mut entries: Vec<ScoredEntry>,
    control: &StorageReadControl,
) -> Result<DocumentSupport, SQLError> {
    control.check().map_err(|error| {
        crate::storage_errors::storage_error("retain retrieval support", &error)
    })?;
    entries.sort_unstable_by_key(|entry| entry.doc_id);
    entries.dedup_by_key(|entry| entry.doc_id);
    let memory = control.memory();
    let available = memory.limit().saturating_sub(memory.used());
    // Leave space for subsequent leaves and their spill workspace under the same command allowance.
    if entries.len().saturating_mul(size_of::<DocId>()) <= available / 2 {
        let mut ids = BudgetedVec::new(memory);
        ids.reserve(entries.len())?;
        for entry in entries {
            control.cancellation().check()?;
            ids.push(entry.doc_id)?;
        }
        return Ok(DocumentSupport::Resident(ids));
    }
    // Charge the staging block and encrypted file's block workspace before opening it. The canonical retrieval API already owns `entries`; only document identities survive this conversion.
    let workspace = memory.reserve(4 * BLOCK)?;
    let mut file = BlockTemporaryFile::<BLOCK>::new().map_err(io)?;
    let mut block = [0_u8; BLOCK];
    for batch in entries.chunks(BLOCK / size_of::<DocId>()) {
        control.cancellation().check()?;
        for (bytes, entry) in block.chunks_exact_mut(8).zip(batch) {
            bytes.copy_from_slice(&entry.doc_id.to_be_bytes());
        }
        file.write_all(&block[..batch.len() * 8]).map_err(io)?;
    }
    Ok(DocumentSupport::Spilled {
        file: RefCell::new(file),
        len: entries.len() as u64,
        _workspace: workspace,
    })
}

pub(super) fn contains(
    rows: &DocumentSupport,
    doc_id: DocId,
    control: &StorageReadControl,
) -> Result<bool, SQLError> {
    control.cancellation().check()?;
    match rows {
        DocumentSupport::Resident(ids) => Ok(ids.binary_search(&doc_id).is_ok()),
        DocumentSupport::Spilled { file, len, .. } => {
            let mut file = file.borrow_mut();
            let (mut left, mut right) = (0, *len);
            while left < right {
                control.cancellation().check()?;
                let middle = left + (right - left) / 2;
                file.seek(SeekFrom::Start(middle * 8)).map_err(io)?;
                let mut bytes = [0; 8];
                file.read_exact(&mut bytes).map_err(io)?;
                match u64::from_be_bytes(bytes).cmp(&doc_id) {
                    std::cmp::Ordering::Equal => return Ok(true),
                    std::cmp::Ordering::Less => left = middle + 1,
                    std::cmp::Ordering::Greater => right = middle,
                }
            }
            Ok(false)
        }
    }
}
