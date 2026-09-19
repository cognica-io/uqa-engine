//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Probe disjoint intervals within each key length, then merge their ordered candidates.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{suffix, Bounds, PhysicalResult, MAX_KEY};

pub(in crate::mvcc) fn next_key(
    connection: &Connection,
    lower: &[u8],
    exclusive: bool,
    upper: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let _bindings = crate::read_control::reserve_bindings(control, &[lower])?;
    let mut lengths = connection.prepare(
        "SELECT key_length FROM _uqa_mvcc_runs WHERE key_length > ?1 ORDER BY key_length LIMIT 1",
    )?;
    let mut predecessor = connection.prepare(super::LOOKUP)?;
    let mut successor = connection.prepare("SELECT first_key, last_key, sequence, kind, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END, key_length FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key > ?2 ORDER BY first_key LIMIT 1")?;
    let mut previous = 0;
    let mut selected: Option<BudgetedVec<u8>> = None;
    while let Some(length) = lengths
        .query_row([previous], |row| row.get::<_, i64>(0))
        .optional()?
    {
        control.cancellation().check().map_err(VersionError::from)?;
        if !(8..=1024).contains(&length) {
            return Err(VersionError::InvalidEncoding("invalid record run key length").into());
        }
        previous = length;
        let mut candidate = None;
        {
            let mut rows = predecessor.query(params![length, lower])?;
            if let Some(row) = rows.next()? {
                candidate = next_member(&Bounds::decode(row)?, lower, exclusive);
            }
        }
        if candidate.is_none() {
            let mut rows = successor.query(params![length, lower])?;
            if let Some(row) = rows.next()? {
                candidate = Some(Bounds::decode(row)?.first);
            }
        }
        if let Some(key) = candidate {
            let key = &key[..usize::try_from(length).expect("validated length")];
            if upper.is_none_or(|upper| key < upper)
                && selected.as_deref().is_none_or(|selected| key < selected)
            {
                selected = Some(crate::read_control::copy_bytes(key, 0, control)?);
            }
        }
    }
    Ok(selected)
}

fn next_member(bounds: &Bounds, lower: &[u8], exclusive: bool) -> Option<[u8; MAX_KEY]> {
    let start = suffix(bounds.first());
    let count = suffix(bounds.last()) - start + 1;
    let mut key = bounds.first;
    let offset = bounds.key_length - 8;
    let mut low = 0;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        key[offset..bounds.key_length].copy_from_slice(&(start + middle).to_be_bytes());
        let candidate = &key[..bounds.key_length];
        if candidate < lower || (exclusive && candidate == lower) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    if low == count {
        return None;
    }
    key[offset..bounds.key_length].copy_from_slice(&(start + low).to_be_bytes());
    Some(key)
}
