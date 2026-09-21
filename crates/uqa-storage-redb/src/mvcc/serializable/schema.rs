//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic keyed checkpoints preserve old singleton state until migration commits.

use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableHandle};
use uqa_storage::{
    mvcc::{SerializableGraph, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::super::{codec, physical_writer, RedbRecordStore, METADATA};
use crate::error::redb_error;

const LEGACY: TableDefinition<u8, &[u8]> = TableDefinition::new("uqa_mvcc_serializable");
const RECORDS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("uqa_mvcc_serializable_records");
const MARKER: &str = "serializable";
const MAGIC: &[u8; 8] = b"UQARED02";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Empty,
    Singleton,
    Records,
}

pub(super) struct Loaded {
    pub(super) graph: SerializableGraph,
    format: Format,
}

pub(super) fn load(store: &RedbRecordStore, control: &StorageReadControl) -> VersionResult<Loaded> {
    control.check()?;
    let transaction = store.database.begin_read().map_err(redb_error)?;
    let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
    codec::validate_metadata(&metadata, store.identity)?;
    let marker = metadata.get(MARKER).map_err(redb_error)?;
    let legacy = optional_table(transaction.open_table(LEGACY))?;
    let records = optional_table(transaction.open_table(RECORDS))?;
    let Some(marker) = marker else {
        if legacy.is_some() || records.is_some() {
            return Err(invalid());
        }
        let mut coordinator = [0; 16];
        getrandom::fill(&mut coordinator)
            .map_err(|error| redb_error(std::io::Error::other(error.to_string())))?;
        return Ok(Loaded {
            graph: SerializableGraph::new(store.identity, coordinator, control.memory())?,
            format: Format::Empty,
        });
    };
    let (format, coordinator) = decode_marker(marker.value())?;
    let graph = match (format, legacy, records) {
        (Format::Singleton, Some(table), None) => {
            if table.len().map_err(redb_error)? != 1 {
                return Err(invalid());
            }
            let checkpoint = table.get(0).map_err(redb_error)?.ok_or_else(invalid)?;
            SerializableGraph::read_checkpoint(
                store.identity,
                coordinator,
                &mut checkpoint.value(),
                control,
            )?
        }
        (Format::Records, None, Some(table)) => SerializableGraph::read_checkpoint_records(
            store.identity,
            coordinator,
            control,
            |visit| {
                for row in table.iter().map_err(redb_error)? {
                    control.check()?;
                    let (key, value) = row.map_err(redb_error)?;
                    visit(key.value(), &mut value.value())?;
                }
                Ok(())
            },
        )?,
        _ => return Err(invalid()),
    };
    Ok(Loaded { graph, format })
}

pub(super) fn persist(
    store: &RedbRecordStore,
    loaded: &Loaded,
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.check()?;
    if !loaded.graph.checkpoint_records_changed() {
        return Ok(());
    }
    let transaction = physical_writer(&store.database)?;
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        match metadata.get(MARKER).map_err(redb_error)? {
            Some(marker)
                if decode_marker(marker.value())?
                    == (loaded.format, loaded.graph.coordinator()) => {}
            None if loaded.format == Format::Empty => {}
            _ => return Err(invalid()),
        }
        let mut present = (false, false);
        for table in transaction.list_tables().map_err(redb_error)? {
            present.0 |= table.name() == LEGACY.name();
            present.1 |= table.name() == RECORDS.name();
        }
        if present
            != match loaded.format {
                Format::Empty => (false, false),
                Format::Singleton => (true, false),
                Format::Records => (false, true),
            }
        {
            return Err(invalid());
        }
        if loaded.format == Format::Singleton {
            transaction.delete_table(LEGACY).map_err(redb_error)?;
        }
        let mut records = transaction.open_table(RECORDS).map_err(redb_error)?;
        loaded
            .graph
            .write_checkpoint_changes(control, |key, record| {
                let key = key.as_bytes().as_slice();
                let Some(record) = record else {
                    if records.remove(key).map_err(redb_error)?.is_none() {
                        return Err(invalid());
                    }
                    return Ok(());
                };
                let length =
                    usize::try_from(record.encoded_length(control)?).map_err(|_| invalid())?;
                // redb's reserved value allocates a zero-filled temporary; charge one changed record before entering the driver, then stream into its page.
                let _workspace = control.memory().reserve(length)?;
                let mut value = records.insert_reserve(key, length).map_err(redb_error)?;
                record.write(&mut std::io::Cursor::new(value.as_mut()), control)
            })?;
        if loaded.format != Format::Records {
            let mut marker = [0; 24];
            marker[..8].copy_from_slice(MAGIC);
            marker[8..].copy_from_slice(&loaded.graph.coordinator());
            metadata
                .insert(MARKER, marker.as_slice())
                .map_err(redb_error)?;
        }
    }
    transaction.commit().map_err(redb_error)?;
    Ok(())
}

fn optional_table<T>(result: Result<T, redb::TableError>) -> VersionResult<Option<T>> {
    match result {
        Ok(table) => Ok(Some(table)),
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
        Err(error) => Err(redb_error(error).into()),
    }
}

fn decode_marker(marker: &[u8]) -> VersionResult<(Format, [u8; 16])> {
    if marker.len() != 24 {
        return Err(invalid());
    }
    let format = match &marker[..8] {
        b"UQARED01" => Format::Singleton,
        magic if magic == MAGIC => Format::Records,
        _ => return Err(invalid()),
    };
    Ok((format, marker[8..].try_into().map_err(|_| invalid())?))
}

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid serializable redb state")
}
