//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent identifier reservations use the same redb write admission as record commits.

use redb::{ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use uqa_storage::mvcc::{
    reserve_identifier_workspace, IdentifierAllocation, IdentifierRequest, VersionError,
    VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, physical_writer, redb_error, RedbRecordStore, METADATA};

pub(super) const TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("uqa_mvcc_identifiers");

fn validate_metadata(
    store: &RedbRecordStore,
    metadata: &impl ReadableTable<&'static str, &'static [u8]>,
) -> VersionResult<()> {
    if codec::database_id(metadata)? != store.identity {
        return Err(VersionError::WrongDatabase);
    }
    if codec::read_u64(metadata, "format")? != 9 {
        return Err(VersionError::InvalidEncoding("unknown record format"));
    }
    Ok(())
}

pub(super) fn read(
    store: &RedbRecordStore,
    namespace: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<u64>> {
    let _workspace = reserve_identifier_workspace(namespace, control)?;
    let transaction = store.database.begin_read().map_err(redb_error)?;
    validate_metadata(
        store,
        &transaction.open_table(METADATA).map_err(redb_error)?,
    )?;
    let identifiers = transaction.open_table(TABLE).map_err(redb_error)?;
    let watermark = identifiers
        .get(namespace)
        .map_err(redb_error)?
        .map(|value| codec::decode_u64(value.value()))
        .transpose()?;
    control.cancellation().check()?;
    Ok(watermark)
}

pub(super) fn allocate(
    store: &RedbRecordStore,
    namespace: &[u8],
    request: IdentifierRequest,
    control: &StorageReadControl,
) -> VersionResult<IdentifierAllocation> {
    let _workspace = request.reserve_workspace(namespace, control)?;
    let transaction = physical_writer(&store.database)?;
    let allocation = {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        validate_metadata(store, &metadata)?;
        let mut present = false;
        for table in transaction.list_tables().map_err(redb_error)? {
            present |= table.name() == TABLE.name();
        }
        if !present {
            return Err(VersionError::InvalidEncoding(
                "missing identifier watermark table",
            ));
        }
        let mut identifiers = transaction.open_table(TABLE).map_err(redb_error)?;
        let previous = identifiers
            .get(namespace)
            .map_err(redb_error)?
            .map(|value| codec::decode_u64(value.value()))
            .transpose()?;
        let allocation = request.prepare(previous)?;
        if previous != Some(allocation.watermark()) {
            identifiers
                .insert(namespace, allocation.watermark().to_be_bytes().as_slice())
                .map_err(redb_error)?;
        }
        allocation
    };
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(allocation)
}
