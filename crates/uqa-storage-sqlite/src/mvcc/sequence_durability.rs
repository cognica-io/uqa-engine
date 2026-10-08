//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Autonomous sequence visibility and the durable prefix required by its consumers.

use rusqlite::Connection;
use uqa_storage::{
    mvcc::{DatabaseId, PreparedRecordCommit},
    read_control::StorageReadControl,
};

use super::{admission, codec, native, PhysicalResult};

/// Only replacements of existing native sequence value records may defer their WAL sync. Mixed data/definition writes, removals and direct record callers retain ordinary FULL publication.
pub(super) fn eligible(
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    if prepared.is_empty() {
        return Ok(false);
    }
    let mut writes = prepared.writes();
    while let Some(record) = writes.next_metadata(control)? {
        if !record.live()
            || record.expected().is_none()
            || !matches!(native::NativeRecordIdentity::decode(record.key()), Ok(identity)
                if identity.family() == native::NativeRecordFamily::SequenceValues)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A FULL metadata-only transaction synchronizes the complete earlier WAL prefix without creating a new record revision. The certificate is never advanced by an unsynchronized publication.
pub(super) fn synchronize(
    connection: &Connection,
    database: DatabaseId,
    namespace: Option<native::NativeRecordNamespace>,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let current = admission::retry(connection, true, control, || {
        let read = connection.unchecked_transaction()?;
        native::check_mapping(&read, namespace)?;
        let current = codec::header(&read, database)?;
        control
            .check()
            .map_err(uqa_storage::mvcc::VersionError::from)?;
        read.commit()?;
        Ok(current)
    })?;
    if current.sequence_durable == current.sequence {
        return Ok(());
    }
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, namespace)?;
    let current = codec::header(&transaction, database)?;
    if current.sequence_durable == current.sequence {
        return Ok(());
    }
    transaction.execute(
        "UPDATE _uqa_mvcc_metadata SET sequence_durable = sequence WHERE singleton = 1",
        [],
    )?;
    admission::commit(transaction, control)
}
