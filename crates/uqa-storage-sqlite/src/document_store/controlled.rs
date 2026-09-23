//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native document decoding reserves payloads before allocation and transfers their leases to readers.

use uqa_core::{
    json::JsonReadError,
    memory::{Budgeted, BudgetedVec},
    Value, ValueRetentionError,
};
use uqa_storage::read_control::StorageReadControl;

use super::{DocId, SQLiteError};

mod blob;
mod container;
mod identifiers;
mod typed;

pub(super) use blob::{decode_blob, marker, Marker};

fn retention_error(error: ValueRetentionError) -> JsonReadError {
    match error {
        ValueRetentionError::Memory(error) => JsonReadError::Memory(error),
        ValueRetentionError::Cancelled(error) => JsonReadError::Cancelled(error),
    }
}

pub(super) fn read_error(error: JsonReadError) -> SQLiteError {
    match error {
        JsonReadError::Memory(error) => SQLiteError::Memory(error),
        JsonReadError::Cancelled(error) => SQLiteError::Cancelled(error),
        JsonReadError::InvalidJson => {
            SQLiteError::StorageBackend("invalid persisted document value".into())
        }
    }
}

pub(super) fn corrupt(table: &str, doc_id: DocId, field: &str, reason: &str) -> SQLiteError {
    SQLiteError::CorruptDocumentBlob {
        table: table.to_owned(),
        doc_id,
        field: field.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests;
