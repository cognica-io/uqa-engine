//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered metadata joins locate admitted payloads by their stable physical address.

use uqa_storage::read_control::StorageReadControl;

use super::{
    codec, ordered, params, prefix_upper_bound, reserve_bindings, value_from_info, BorrowedRecord,
    CommitSequence, Connection, PhysicalResult, RecordScanVisitor, VersionError,
};

pub(super) fn visit(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    boundary: CommitSequence,
    control: &StorageReadControl,
    visitor: &mut RecordScanVisitor<'_>,
) -> PhysicalResult<()> {
    let upper = prefix_upper_bound(prefix, control)?;
    let after = after.filter(|after| *after >= prefix);
    let lower = after.unwrap_or(prefix);
    let _bindings = reserve_bindings(control, &[lower, upper.as_deref().unwrap_or_default()])?;
    let sql = payload_statement(after.is_some(), upper.is_some());
    let mut statement = connection.prepare_cached(sql)?;
    let boundary_bytes = boundary.as_u64().to_be_bytes();
    let mut payloads =
        statement.query(params![lower, upper.as_deref(), boundary_bytes.as_slice()])?;
    let mut previous_payload = None;
    let result = ordered::visit_sources(
        connection,
        prefix,
        after,
        limit,
        boundary,
        control,
        &mut |key, source| {
            if let ordered::RecordSource::Run(info) = source {
                let mut more = true;
                value_from_info(
                    connection,
                    key,
                    info,
                    boundary,
                    usize::MAX,
                    control,
                    &mut |record| {
                        more = visitor(key, record.expect("selected visible record"))?;
                        Ok(())
                    },
                )?;
                return Ok(more);
            }
            let ordered::RecordSource::Point(info) = source else {
                unreachable!()
            };
            control.cancellation().check().map_err(VersionError::from)?;
            let length = info.and_then(|info| info.length);
            let bytes = length
                .unwrap_or(0)
                .checked_add(key.len())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
                .map_err(VersionError::from)?;
            let payload = control
                .memory()
                .reserve(bytes)
                .map_err(VersionError::from)?;
            // The metadata cursor admits both fields before this cursor steps or obtains BLOB pointers.
            let row = payloads.next()?.ok_or(VersionError::InvalidEncoding(
                "version disappeared within a read",
            ))?;
            previous_payload = Some(payload);
            if codec::bytes(row, 0)? != key {
                return Err(VersionError::InvalidEncoding(
                    "version identity changed within a read",
                )
                .into());
            }
            let value = match row.get_ref(1)? {
                rusqlite::types::ValueRef::Null => None,
                _ => Some(codec::bytes(row, 1)?),
            };
            if value.map(<[u8]>::len) != length {
                return Err(VersionError::InvalidEncoding(
                    "version size or type changed within a read",
                )
                .into());
            }
            control.cancellation().check().map_err(VersionError::from)?;
            let Some(info) = info else {
                return Ok(true);
            };
            let more = visitor(
                key,
                BorrowedRecord {
                    revision: Some(CommitSequence::from_u64(info.revision)),
                    value,
                },
            )?;
            control.cancellation().check().map_err(VersionError::from)?;
            Ok(more)
        },
    );
    // Finalize the last borrowed SQLite row before releasing its allowance, including on errors.
    drop(payloads);
    drop(statement);
    drop(previous_payload);
    result
}

fn payload_statement(exclusive: bool, bounded: bool) -> &'static str {
    macro_rules! payloads {
        ($predicate:literal) => {
            concat!(
                "SELECT h.key, CASE WHEN h.sequence <= ?3 THEN ",
                "CASE WHEN h.compacted = 1 THEN NULL ELSE v.value END ",
                "ELSE (SELECT p.value FROM _uqa_mvcc_version_metadata m ",
                "JOIN _uqa_mvcc_versions p ON p.version_id = m.version_id ",
                "AND p.key = m.key AND p.sequence = m.sequence WHERE m.key = h.key ",
                "AND m.sequence <= ?3 ORDER BY m.sequence DESC LIMIT 1) END ",
                "FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_version_metadata m ",
                "ON m.key = h.key AND m.sequence = h.sequence ",
                "LEFT JOIN _uqa_mvcc_versions v ON v.version_id = m.version_id ",
                "AND v.key = h.key AND v.sequence = h.sequence WHERE ",
                $predicate,
                " ORDER BY h.key"
            )
        };
    }
    match (exclusive, bounded) {
        (true, true) => payloads!("h.key > ?1 AND h.key < ?2"),
        (true, false) => payloads!("h.key > ?1 AND (?2 IS NULL)"),
        (false, true) => payloads!("h.key >= ?1 AND h.key < ?2"),
        (false, false) => payloads!("h.key >= ?1 AND (?2 IS NULL)"),
    }
}

#[cfg(test)]
mod tests;
