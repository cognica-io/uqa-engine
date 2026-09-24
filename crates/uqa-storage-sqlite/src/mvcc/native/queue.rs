//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Page transaction-local SQL work queues without retaining a database-sized Rust collection.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{invalid, NativeRecordFamily};
use crate::mvcc::{codec, PhysicalResult};

pub(super) fn visit(
    connection: &Connection,
    table: &'static str,
    condition: &str,
    control: &StorageReadControl,
    mut visit: impl FnMut(NativeRecordFamily, &[u8]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    let mut family: Option<u16> = None;
    let mut key: Option<BudgetedVec<u8>> = None;
    loop {
        control.cancellation().check().map_err(VersionError::from)?;
        let bounds = if family.is_some() {
            "(family, physical_key) > (?1, ?2)"
        } else {
            "?1 IS NULL AND ?2 IS NULL"
        };
        let suffix = format!(
            "FROM {table} WHERE ({condition}) AND {bounds} ORDER BY family, physical_key LIMIT 1"
        );
        let _bindings =
            crate::read_control::reserve_bindings(control, &[key.as_deref().unwrap_or_default()])?;
        let info: Option<(u16, i64)> = connection
            .query_row(
                &format!("SELECT family, length(physical_key) {suffix}"),
                params![family, key.as_deref()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((next_family, length)) = info else {
            break;
        };
        let length =
            usize::try_from(length).map_err(|_| invalid("invalid native work queue key length"))?;
        let _payload = control
            .memory()
            .reserve(length)
            .map_err(VersionError::from)?;
        let mut statement = connection.prepare(&format!("SELECT physical_key {suffix}"))?;
        let mut rows = statement.query(params![family, key.as_deref()])?;
        let row = rows
            .next()?
            .ok_or_else(|| invalid("native work queue changed within its transaction"))?;
        let bytes = codec::bytes(row, 0)?;
        if bytes.len() != length {
            return Err(invalid("native work queue key changed within its transaction").into());
        }
        let mut next_key = BudgetedVec::new(control.memory());
        next_key
            .extend_from_slice(bytes)
            .map_err(VersionError::from)?;
        drop(rows);
        drop(statement);
        let parsed = NativeRecordFamily::from_id(next_family)
            .ok_or_else(|| invalid("unknown native work queue family"))?;
        visit(parsed, &next_key)?;
        family = Some(next_family);
        key = Some(next_key);
    }
    Ok(())
}
