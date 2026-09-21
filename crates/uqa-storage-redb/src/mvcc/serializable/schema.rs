//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A singleton checkpoint and its metadata marker publish atomically with immediate durability.

use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableHandle};
use uqa_storage::{
    mvcc::{SerializableGraph, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::super::{codec, physical_writer, RedbRecordStore, METADATA};
use crate::error::redb_error;

const TABLE: TableDefinition<u8, &[u8]> = TableDefinition::new("uqa_mvcc_serializable");
const MARKER: &str = "serializable";
const MAGIC: &[u8; 8] = b"UQARED01";

pub(super) struct Loaded {
    pub(super) graph: SerializableGraph,
    initialized: bool,
}

pub(super) fn load(store: &RedbRecordStore, control: &StorageReadControl) -> VersionResult<Loaded> {
    control.cancellation().check()?;
    let transaction = store.database.begin_read().map_err(redb_error)?;
    let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
    codec::validate_metadata(&metadata, store.identity)?;
    let marker = metadata.get(MARKER).map_err(redb_error)?;
    let table = transaction.open_table(TABLE);
    match (marker, table) {
        (None, Err(redb::TableError::TableDoesNotExist(_))) => {
            let mut coordinator = [0; 16];
            getrandom::fill(&mut coordinator)
                .map_err(|error| redb_error(std::io::Error::other(error.to_string())))?;
            Ok(Loaded {
                graph: SerializableGraph::new(store.identity, coordinator, control.memory())?,
                initialized: false,
            })
        }
        (Some(marker), Ok(table)) => {
            let coordinator = decode_marker(marker.value())?;
            if table.len().map_err(redb_error)? != 1 {
                return Err(invalid());
            }
            let checkpoint = table.get(0).map_err(redb_error)?.ok_or_else(invalid)?;
            Ok(Loaded {
                graph: SerializableGraph::read_checkpoint(
                    store.identity,
                    coordinator,
                    &mut checkpoint.value(),
                    control,
                )?,
                initialized: true,
            })
        }
        (_, Err(error)) if !matches!(error, redb::TableError::TableDoesNotExist(_)) => {
            Err(redb_error(error).into())
        }
        _ => Err(invalid()),
    }
}

pub(super) fn persist(
    store: &RedbRecordStore,
    loaded: &Loaded,
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.check()?;
    if !loaded.graph.checkpoint_changed() {
        return Ok(());
    }
    let length =
        usize::try_from(loaded.graph.checkpoint_length(control)?).map_err(|_| invalid())?;
    // redb 4.1 insert_reserve creates a temporary zero-filled value before returning a mutable page. Charge that allocation before entering the driver; encoding then streams into the page without a separate Vec.
    let _workspace = control.memory().reserve(length)?;
    let transaction = physical_writer(&store.database)?;
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        match metadata.get(MARKER).map_err(redb_error)? {
            Some(marker)
                if loaded.initialized
                    && decode_marker(marker.value())? == loaded.graph.coordinator() => {}
            None if !loaded.initialized => {}
            _ => return Err(invalid()),
        }
        let present = transaction
            .list_tables()
            .map_err(redb_error)?
            .any(|table| table.name() == TABLE.name());
        if present != loaded.initialized {
            return Err(invalid());
        }
        let mut table = transaction.open_table(TABLE).map_err(redb_error)?;
        let mut value = table.insert_reserve(0, length).map_err(redb_error)?;
        loaded
            .graph
            .write_checkpoint(&mut std::io::Cursor::new(value.as_mut()), control)?;
        let mut marker = [0; 24];
        marker[..8].copy_from_slice(MAGIC);
        marker[8..].copy_from_slice(&loaded.graph.coordinator());
        metadata
            .insert(MARKER, marker.as_slice())
            .map_err(redb_error)?;
    }
    transaction.commit().map_err(redb_error)?;
    Ok(())
}

fn decode_marker(marker: &[u8]) -> VersionResult<[u8; 16]> {
    if marker.len() != 24 || &marker[..8] != MAGIC {
        return Err(invalid());
    }
    marker[8..].try_into().map_err(|_| invalid())
}

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid serializable redb state")
}
