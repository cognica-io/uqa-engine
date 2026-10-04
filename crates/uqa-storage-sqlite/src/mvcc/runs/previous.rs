//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Seek the greatest member of disjoint runs without expanding their contents.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{suffix, Bounds, PhysicalResult, MAX_KEY};

pub(in crate::mvcc) fn previous_key(
    connection: &Connection,
    prefix: &[u8],
    before: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let _bindings = crate::read_control::reserve_bindings(control, &[before.unwrap_or_default()])?;
    let mut lengths = connection.prepare_cached(
        "SELECT key_length FROM _uqa_mvcc_runs WHERE key_length > ?1 ORDER BY key_length LIMIT 1",
    )?;
    let sql = if before.is_some() {
        "SELECT first_key, last_key, sequence, kind, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END, key_length FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key < ?2 ORDER BY first_key DESC LIMIT 1"
    } else {
        "SELECT first_key, last_key, sequence, kind, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END, key_length FROM _uqa_mvcc_runs WHERE key_length = ?1 AND (?2 IS NULL) ORDER BY first_key DESC LIMIT 1"
    };
    let mut predecessor = connection.prepare_cached(sql)?;
    let mut previous = 0;
    let mut selected: Option<BudgetedVec<u8>> = None;
    while let Some(length) = lengths
        .query_row([previous], |row| row.get::<_, i64>(0))
        .optional()?
    {
        control.check().map_err(VersionError::from)?;
        if !(8..=MAX_KEY as i64).contains(&length) {
            return Err(VersionError::InvalidEncoding("invalid record run key length").into());
        }
        previous = length;
        let mut rows = predecessor.query(params![length, before])?;
        let Some(row) = rows.next()? else { continue };
        let bounds = Bounds::decode(row)?;
        let Some(key) = previous_member(&bounds, before) else {
            continue;
        };
        let key = &key[..bounds.key_length];
        if key.starts_with(prefix) && selected.as_deref().is_none_or(|selected| key > selected) {
            selected = Some(crate::read_control::copy_bytes(key, 0, control)?);
        }
    }
    Ok(selected)
}

fn previous_member(bounds: &Bounds, before: Option<&[u8]>) -> Option<[u8; MAX_KEY]> {
    let Some(before) = before else {
        return Some(bounds.last);
    };
    let start = suffix(bounds.first());
    let count = suffix(bounds.last()) - start + 1;
    let mut key = bounds.first;
    let offset = bounds.key_length - 8;
    let mut low = 0;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        key[offset..bounds.key_length].copy_from_slice(&(start + middle).to_be_bytes());
        if &key[..bounds.key_length] < before {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let last = low.checked_sub(1)?;
    key[offset..bounds.key_length].copy_from_slice(&(start + last).to_be_bytes());
    Some(key)
}
