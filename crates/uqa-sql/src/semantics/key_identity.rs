//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The physical document identity an integer primary-key value names.

use crate::{ast::ColumnDef, SQLError};
use uqa_core::{DocId, Value};

/// The unique primary-key column when its declared type is an integer. Tables without a key, with multiple key columns or with a noninteger key do not select direct identity mapping.
#[must_use]
pub fn single_integer_primary_key(columns: &[ColumnDef]) -> Option<&str> {
    let mut key = None;
    for column in columns {
        if !select(&mut key, column) {
            return None;
        }
    }
    key_name(key)
}

/// Inspect at most one declared column between checkpoints without copying a name or evaluating a default expression. Both original and invoking cancellation remain active; the selected name borrows the original declaration.
pub async fn single_integer_primary_key_from<'a>(
    columns: &'a [ColumnDef],
    original: &uqa_core::CancellationToken,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<Option<&'a str>, SQLError> {
    let control =
        uqa_core::memory::ProductionControl::new(input.memory, original, input.cancellation);
    control.check_cancellation()?;
    let mut key = None;
    for column in columns {
        control.check_cancellation()?;
        input.checkpoint().await?;
        control.check_cancellation()?;
        if !select(&mut key, column) {
            return Ok(None);
        }
    }
    let Some(key) = key else {
        return Ok(None);
    };
    let mut ty = &key.ty;
    while let crate::ColumnType::Domain { base, .. } = ty {
        control.check_cancellation()?;
        input.checkpoint().await?;
        control.check_cancellation()?;
        ty = base;
    }
    Ok(ty.is_integer().then_some(key.name.as_str()))
}

fn select<'a>(key: &mut Option<&'a ColumnDef>, column: &'a ColumnDef) -> bool {
    if column.primary_key {
        if key.is_some() {
            return false;
        }
        *key = Some(column);
    }
    true
}

fn key_name(key: Option<&ColumnDef>) -> Option<&str> {
    key.filter(|column| column.ty.is_integer())
        .map(|column| column.name.as_str())
}

/// A table whose single primary-key column is an integer names each row whose key lies in `0..KEY_IDENTITY_LIMIT` by the document identity equal to the key, so a key lookup reads that one identity. Its other rows, with negative keys or keys at or above the limit, take identities the table generates at or above the limit, where no key value names one; a key lookup reaches them through the key's index.
pub const KEY_IDENTITY_LIMIT: DocId = 1 << 62;

/// The document identity an integer primary-key value names, or `None` for a value no identity is named by.
#[must_use]
pub fn key_document_id(value: &Value) -> Option<DocId> {
    match value {
        Value::Int(key) => DocId::try_from(*key)
            .ok()
            .filter(|doc_id| *doc_id < KEY_IDENTITY_LIMIT),
        _ => None,
    }
}

/// Whether `doc_id` is an identity an integer primary-key value names.
#[must_use]
pub const fn is_key_document_id(doc_id: DocId) -> bool {
    doc_id < KEY_IDENTITY_LIMIT
}

#[cfg(test)]
mod tests;
