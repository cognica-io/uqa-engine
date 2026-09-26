//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent identifier reservations use the same redb write admission as record commits.

use redb::{ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use uqa_storage::key_value::diskann_identifiers;
use uqa_storage::mvcc::{
    reserve_identifier_workspace, IdentifierAllocation, IdentifierRequest, VersionError,
    VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, physical_writer, redb_error, RedbRecordStore, METADATA};

pub(super) const TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("uqa_mvcc_identifiers");

/// Consolidation shares the caller's atomic format upgrade; provider page caches own physical I/O.
pub(super) fn consolidate_diskann_generations(
    table: &mut redb::Table<'_, &'static [u8], &'static [u8]>,
) -> VersionResult<()> {
    use diskann_identifiers::{LEGACY_END, LEGACY_NAMESPACE_BYTES, LEGACY_PREFIX, NAMESPACE};
    let previous = table
        .get(NAMESPACE)
        .map_err(redb_error)?
        .map(|value| codec::decode_u64(value.value()))
        .transpose()?;
    let mut maximum = previous;
    let mut after: Option<[u8; LEGACY_NAMESPACE_BYTES]> = None;
    loop {
        let mut keys = [[0_u8; LEGACY_NAMESPACE_BYTES]; 64];
        let mut count = 0;
        let begin = after
            .as_ref()
            .map_or(LEGACY_PREFIX.as_slice(), |key| key.as_slice());
        for entry in table
            .range(begin..LEGACY_END.as_slice())
            .map_err(redb_error)?
        {
            let (key, value) = entry.map_err(redb_error)?;
            if !diskann_identifiers::is_legacy_namespace(key.value()) {
                continue;
            }
            keys[count].copy_from_slice(key.value());
            let value = codec::decode_u64(value.value())?;
            maximum = Some(maximum.map_or(value, |old| old.max(value)));
            count += 1;
            if count == keys.len() {
                break;
            }
        }
        if count == 0 {
            break;
        }
        for key in &keys[..count] {
            table.remove(key.as_slice()).map_err(redb_error)?;
        }
        after = Some(keys[count - 1]);
    }
    if maximum != previous {
        let value = maximum.expect("only observed reservations change the maximum");
        table
            .insert(NAMESPACE, value.to_be_bytes().as_slice())
            .map_err(redb_error)?;
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
    codec::validate_metadata(
        &transaction.open_table(METADATA).map_err(redb_error)?,
        store.identity,
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
        codec::validate_metadata(&metadata, store.identity)?;
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
