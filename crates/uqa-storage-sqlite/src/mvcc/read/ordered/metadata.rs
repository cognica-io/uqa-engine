//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A bounded scalar carries the selected revision and size out of one predecessor seek.

use rusqlite::{types::ValueRef, Row};

use super::{codec, CommitSequence, Info, PhysicalResult, VersionError};

// Sixteen hexadecimal digits, a separator and at most nineteen decimal size digits.
pub(super) const MAX_BYTES: usize = 36;

// Key selection retains the same predecessor and corruption checks.
macro_rules! statement {
    ($keys:literal, $predicate:literal) => {
        concat!(
            "SELECT h.sequence, h.compacted, (SELECT CASE ",
            "WHEN typeof(sequence) != 'blob' THEN -3 ",
            "WHEN length(sequence) != 8 THEN -4 ",
            "WHEN h.sequence <= ?3 THEN CASE WHEN sequence != h.sequence THEN -5 ",
            "WHEN payload_length IS NULL THEN -1 WHEN payload_length >= 0 THEN payload_length ELSE -2 END ",
            "ELSE hex(sequence) || ':' || coalesce(payload_length, 'n') END ",
            "FROM _uqa_mvcc_version_metadata WHERE key = h.key AND sequence <= ?3 ",
            "ORDER BY sequence DESC LIMIT 1), ",
            $keys,
            " FROM _uqa_mvcc_heads h WHERE ",
            $predicate,
            " ORDER BY h.key"
        )
    };
}
pub(super) use statement;

pub(super) fn info(row: &Row<'_>, boundary: CommitSequence) -> PhysicalResult<Option<Info>> {
    let head = codec::decode_head(row)?;
    super::super::checked_point_info(head, boundary, || {
        let bytes = match row.get_ref(2)? {
            ValueRef::Null => return Ok(None),
            ValueRef::Integer(size) => {
                let length = match size {
                    -1 => None,
                    -2 => Some(-1),
                    -3 => {
                        return Err(VersionError::InvalidEncoding("expected a record BLOB").into())
                    }
                    -4 => {
                        return Err(VersionError::InvalidEncoding("invalid sequence length").into())
                    }
                    -5 => {
                        return Err(VersionError::InvalidEncoding(
                            "record head has no matching version",
                        )
                        .into())
                    }
                    size if size >= 0 => Some(size),
                    _ => {
                        return Err(VersionError::InvalidEncoding("invalid version metadata").into())
                    }
                };
                return Ok(Some((head.0.as_u64(), length)));
            }
            ValueRef::Text(bytes) => bytes,
            _ => return Err(VersionError::InvalidEncoding("invalid version metadata").into()),
        };
        if bytes.len() > MAX_BYTES || bytes.get(16) != Some(&b':') || !bytes.is_ascii() {
            return Err(VersionError::InvalidEncoding("invalid version metadata").into());
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| VersionError::InvalidEncoding("invalid version metadata"))?;
        let revision = u64::from_str_radix(&text[..16], 16)
            .map_err(|_| VersionError::InvalidEncoding("invalid version metadata"))?;
        let length = match &text[17..] {
            "n" => None,
            size => Some(
                size.parse::<i64>()
                    .map_err(|_| VersionError::InvalidEncoding("invalid version metadata"))?,
            ),
        };
        Ok(Some((revision, length)))
    })
}
