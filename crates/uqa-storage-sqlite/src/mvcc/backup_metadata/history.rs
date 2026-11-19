//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier generations are published with their watermarks and reclaimed under metadata admission.

use rusqlite::{params, Connection, OptionalExtension};
use uqa_storage::{
    mvcc::{MetadataEpoch, VersionError, RECLAMATION_DOMAIN_PREFIX, RECLAMATION_EPOCH_NAMESPACE},
    read_control::StorageReadControl,
};

use super::super::{codec, PhysicalResult};

pub(in crate::mvcc) const TABLES: [(&str, &str); 3] = [
    ("_uqa_mvcc_backup_metadata", "CREATE TABLE _uqa_mvcc_backup_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), epoch BLOB NOT NULL CHECK(typeof(epoch) = 'blob' AND length(epoch) = 8))"),
    ("_uqa_mvcc_identifier_epochs", "CREATE TABLE _uqa_mvcc_identifier_epochs (namespace BLOB PRIMARY KEY CHECK(typeof(namespace) = 'blob' AND length(namespace) > 0), created BLOB NOT NULL CHECK(typeof(created) = 'blob' AND length(created) = 8), head BLOB NOT NULL CHECK(typeof(head) = 'blob' AND length(head) = 8 AND head >= created)) WITHOUT ROWID"),
    ("_uqa_mvcc_identifier_history", "CREATE TABLE _uqa_mvcc_identifier_history (namespace BLOB NOT NULL CHECK(typeof(namespace) = 'blob' AND length(namespace) > 0), epoch BLOB NOT NULL CHECK(typeof(epoch) = 'blob' AND length(epoch) = 8), watermark BLOB NOT NULL CHECK(typeof(watermark) = 'blob' AND length(watermark) = 8), PRIMARY KEY(namespace, epoch)) WITHOUT ROWID"),
];

/// The predecessor's physical initialization transaction owns this baseline; no captured metadata generation exists yet.
pub(in crate::mvcc) fn initialize(connection: &Connection) -> PhysicalResult<()> {
    connection.execute(
        "INSERT INTO _uqa_mvcc_backup_metadata VALUES (1, x'0000000000000000')",
        [],
    )?;
    connection.execute(
        "INSERT INTO _uqa_mvcc_identifier_epochs SELECT namespace, x'0000000000000000', x'0000000000000000' FROM _uqa_mvcc_identifiers WHERE namespace != ?1 AND substr(namespace, 1, ?2) != ?3",
        params![RECLAMATION_EPOCH_NAMESPACE, RECLAMATION_DOMAIN_PREFIX.len() as i64, RECLAMATION_DOMAIN_PREFIX],
    )?;
    Ok(())
}

pub(super) fn epoch(connection: &Connection) -> PhysicalResult<u64> {
    connection
        .prepare_cached("SELECT epoch FROM _uqa_mvcc_backup_metadata WHERE singleton = 1")?
        .query_row([], |row| Ok(codec::bytes(row, 0).and_then(codec::integer)))?
}

pub(super) fn is_allocation(namespace: &[u8]) -> bool {
    namespace != RECLAMATION_EPOCH_NAMESPACE && !namespace.starts_with(RECLAMATION_DOMAIN_PREFIX)
}

pub(super) fn validate_epochs(created: u64, head: u64, current: u64) -> PhysicalResult<()> {
    if created > head || head > current {
        return Err(VersionError::InvalidEncoding("invalid identifier metadata epochs").into());
    }
    Ok(())
}

pub(in crate::mvcc) fn retain_previous(
    connection: &Connection,
    namespace: &[u8],
    previous: Option<u64>,
) -> PhysicalResult<()> {
    if !is_allocation(namespace) {
        return Ok(());
    }
    let current = epoch(connection)?;
    let head = connection
        .prepare_cached(
            "SELECT created, head FROM _uqa_mvcc_identifier_epochs WHERE namespace = ?1",
        )?
        .query_row([namespace], |row| {
            Ok((
                codec::bytes(row, 0).and_then(codec::integer),
                codec::bytes(row, 1).and_then(codec::integer),
            ))
        })
        .optional()?;
    match (previous, head) {
        (Some(watermark), Some((created, head))) => {
            let (created, head) = (created?, head?);
            validate_epochs(created, head, current)?;
            if head != current {
                connection
                    .prepare_cached("INSERT INTO _uqa_mvcc_identifier_history VALUES (?1, ?2, ?3)")?
                    .execute(params![
                        namespace,
                        head.to_be_bytes().as_slice(),
                        watermark.to_be_bytes().as_slice()
                    ])?;
                connection
                    .prepare_cached(
                        "UPDATE _uqa_mvcc_identifier_epochs SET head = ?2 WHERE namespace = ?1",
                    )?
                    .execute(params![namespace, current.to_be_bytes().as_slice()])?;
            }
        }
        (None, None) => {
            connection
                .prepare_cached("INSERT INTO _uqa_mvcc_identifier_epochs VALUES (?1, ?2, ?2)")?
                .execute(params![namespace, current.to_be_bytes().as_slice()])?;
        }
        _ => {
            return Err(VersionError::InvalidEncoding(
                "identifier metadata head is missing or orphaned",
            )
            .into())
        }
    }
    Ok(())
}

pub(super) fn previous(
    connection: &Connection,
    namespace: &[u8],
    created: u64,
    through: u64,
) -> PhysicalResult<(u64, u64)> {
    connection.prepare_cached("SELECT epoch, watermark FROM _uqa_mvcc_identifier_history WHERE namespace = ?1 AND epoch >= ?2 AND epoch <= ?3 ORDER BY epoch DESC LIMIT 1")?
        .query_row(params![namespace, created.to_be_bytes().as_slice(), through.to_be_bytes().as_slice()], |row| {
            Ok((codec::bytes(row, 0).and_then(codec::integer), codec::bytes(row, 1).and_then(codec::integer)))
        })
        .optional()?
        .ok_or(VersionError::InvalidEncoding("missing retained identifier watermark").into())
        .and_then(|(epoch, watermark)| Ok((epoch?, watermark?)))
}

/// Record admission precedes metadata admission, which precedes the physical writer. Both captures and reclamation use that order.
pub(in crate::mvcc) fn reclaim(
    connection: &Connection,
    oldest: Option<MetadataEpoch>,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let current = epoch(connection)?;
    let horizon = oldest.map_or(current, MetadataEpoch::as_u64);
    if horizon > current {
        return Err(
            VersionError::InvalidEncoding("metadata lease exceeds the current epoch").into(),
        );
    }
    let mut statement = connection.prepare(
        "SELECT namespace, created, head FROM _uqa_mvcc_identifier_epochs ORDER BY namespace",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        control.cancellation().check().map_err(VersionError::from)?;
        let namespace = codec::bytes(row, 0)?;
        let _bindings = crate::read_control::reserve_bindings(control, &[namespace])?;
        let created = codec::bytes(row, 1).and_then(codec::integer)?;
        let head = codec::integer(codec::bytes(row, 2)?)?;
        validate_epochs(created, head, current)?;
        let cutoff = if head <= horizon {
            head
        } else if created <= horizon {
            previous(connection, namespace, created, horizon)?.0
        } else {
            0
        };
        if cutoff == 0 {
            continue;
        }
        loop {
            control.cancellation().check().map_err(VersionError::from)?;
            let removed = connection.prepare_cached("DELETE FROM _uqa_mvcc_identifier_history WHERE namespace = ?1 AND epoch IN (SELECT epoch FROM _uqa_mvcc_identifier_history WHERE namespace = ?1 AND epoch < ?2 ORDER BY epoch LIMIT 128)")?
                .execute(params![namespace, cutoff.to_be_bytes().as_slice()])?;
            if removed < 128 {
                break;
            }
        }
    }
    Ok(())
}
