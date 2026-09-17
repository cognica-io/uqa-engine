//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomically replace the legacy byte table with versioned records and a typed old-writer guard.

use redb::{Database, ReadableTable, TableDefinition, TableHandle, WriteTransaction};
use uqa_storage::mvcc::{CommitSequence, DatabaseId, VersionError, VersionResult};

use super::{codec::read_u64, physical_writer, HEADS, METADATA, VERSIONS};
use crate::error::redb_error;

const LEGACY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_key_value");
const LEGACY_METADATA: TableDefinition<&str, u64> = TableDefinition::new("uqa_storage_metadata");
const GUARD: TableDefinition<u8, u8> = TableDefinition::new("uqa_key_value");

pub(super) fn migrate(database: &Database, identity: DatabaseId) -> VersionResult<()> {
    let transaction = physical_writer(database)?;
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        super::codec::validate_metadata(&metadata, identity)?;
        if metadata
            .get("key_value_format")
            .map_err(redb_error)?
            .is_some()
        {
            if read_u64(&metadata, "key_value_format")? != 1 {
                return Err(VersionError::InvalidEncoding("unknown KeyValue format"));
            }
            verify_guard(&transaction)?;
            return Ok(());
        }

        let mut legacy = false;
        let mut legacy_metadata = false;
        for table in transaction.list_tables().map_err(redb_error)? {
            legacy |= table.name() == LEGACY.name();
            legacy_metadata |= table.name() == LEGACY_METADATA.name();
        }
        if legacy != legacy_metadata {
            return Err(VersionError::InvalidEncoding(
                "incomplete legacy KeyValue table set",
            ));
        }
        if legacy {
            let old_metadata = transaction
                .open_table(LEGACY_METADATA)
                .map_err(redb_error)?;
            let old_sequence = old_metadata
                .get("change_version")
                .map_err(redb_error)?
                .ok_or(VersionError::InvalidEncoding(
                    "missing legacy change version",
                ))?
                .value();
            let current = CommitSequence::from_u64(read_u64(&metadata, "sequence")?);
            let table = transaction.open_table(LEGACY).map_err(redb_error)?;
            let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
            let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
            let mut sequence = current.as_u64().max(old_sequence);
            let mut populated = false;
            for entry in table.iter().map_err(redb_error)? {
                let (key, value) = entry.map_err(redb_error)?;
                let key = key.value();
                if heads.get(key).map_err(redb_error)?.is_some() {
                    return Err(VersionError::InvalidEncoding(
                        "legacy key collides with a versioned record",
                    ));
                }
                if !populated {
                    sequence = sequence.max(current.successor()?.as_u64());
                    populated = true;
                }
                let value = value.value();
                let size = value
                    .len()
                    .checked_add(1)
                    .ok_or(VersionError::InvalidEncoding(
                        "legacy value length overflow",
                    ))?;
                let mut destination = versions
                    .insert_reserve((key, sequence), size)
                    .map_err(redb_error)?;
                destination.as_mut()[0] = 1;
                destination.as_mut()[1..].copy_from_slice(value);
                drop(destination);
                heads.insert(key, sequence).map_err(redb_error)?;
            }
            metadata
                .insert("sequence", sequence.to_be_bytes().as_slice())
                .map_err(redb_error)?;
            drop(versions);
            drop(heads);
            drop(table);
            drop(old_metadata);
            transaction.delete_table(LEGACY).map_err(redb_error)?;
            transaction
                .delete_table(LEGACY_METADATA)
                .map_err(redb_error)?;
        }
        // The released initializer opens this exact name with byte types before it can write.
        transaction
            .open_table(GUARD)
            .map_err(redb_error)?
            .insert(0, 1)
            .map_err(redb_error)?;
        metadata
            .insert("key_value_format", 1_u64.to_be_bytes().as_slice())
            .map_err(redb_error)?;
    }
    transaction.commit().map_err(redb_error)?;
    Ok(())
}

fn verify_guard(transaction: &WriteTransaction) -> VersionResult<()> {
    let present = transaction
        .list_tables()
        .map_err(redb_error)?
        .any(|table| table.name() == GUARD.name());
    if !present {
        return Err(VersionError::InvalidEncoding("missing old-writer guard"));
    }
    let guard = transaction.open_table(GUARD).map_err(redb_error)?;
    if guard.get(0).map_err(redb_error)?.map(|value| value.value()) != Some(1) {
        return Err(VersionError::InvalidEncoding("invalid old-writer guard"));
    }
    Ok(())
}
