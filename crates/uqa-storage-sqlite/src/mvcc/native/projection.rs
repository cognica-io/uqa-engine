//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply evaluated native records and verify every trigger/cascade result before publishing their history. Cache generations are provider-owned additive effects, captured at the same commit sequence.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId, PreparedRecordCommit, VersionError},
    read_control::StorageReadControl,
};

use super::{
    capture::Capture, decode_record, decode_row, invalid, owners, physical, queue, NativeRecord,
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
};
use crate::mvcc::{read, write, Error, PhysicalResult};

// Parents precede their children; native document guards and graph invalidation triggers also run before evaluated index materializations are installed.
const ORDER: [Family; 56] = [
    Family::StandaloneGraphScopes,
    Family::StandaloneGraphMetadata,
    Family::StandaloneGraphCatalog,
    Family::StandaloneGraphVertices,
    Family::StandaloneGraphEdges,
    Family::StandaloneGraphMembership,
    Family::StandaloneGraphLookups,
    Family::TableOwners,
    Family::Schemas,
    Family::Relations,
    Family::Tables,
    Family::Sequences,
    Family::Views,
    Family::ForeignServers,
    Family::ForeignTables,
    Family::CatalogIndexes,
    Family::BtreeIndexes,
    Family::Documents,
    Family::DocumentBlobs,
    Family::BtreeIndexEntries,
    Family::BtreeIndexRepairs,
    Family::Analyzers,
    Family::ColumnStats,
    Family::DocLengths,
    Family::FieldStats,
    Family::GraphVertices,
    Family::GraphEdges,
    Family::NamedGraphs,
    Family::GraphMembership,
    Family::HNSWIndexes,
    Family::HNSWNodes,
    Family::HNSWEdges,
    Family::IVFIndexes,
    Family::IVFCentroids,
    Family::IVFAssignments,
    Family::Metadata,
    Family::Models,
    Family::OccurrenceClusters,
    Family::OccurrenceDocuments,
    Family::OccurrenceFields,
    Family::OccurrenceFormats,
    Family::OccurrenceLengths,
    Family::OccurrenceSkips,
    Family::OccurrenceBlockMax,
    Family::OccurrenceGuards,
    Family::VectorGuards,
    Family::PathIndexes,
    Family::GraphPathIndexState,
    Family::GraphLookups,
    Family::GraphPathPairs,
    Family::PostingClusters,
    Family::PostingDocuments,
    Family::ScoringParams,
    Family::TableFieldAnalyzers,
    Family::Vectors,
    Family::CacheRevisions,
];

pub(in crate::mvcc) fn materialize(
    connection: &Connection,
    database: DatabaseId,
    prepared: &PreparedRecordCommit,
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let capture = Capture::install(connection, control)?;
    capture.resolve(apply(connection, database, prepared, sequence, control))
}

fn apply(
    connection: &Connection,
    database: DatabaseId,
    prepared: &PreparedRecordCommit,
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    seed_originals(connection, database, prepared, control)?;
    seed_targets(connection, prepared, control)?;
    super::sequences::validate_prepared(connection, prepared, control)?;
    queue::visit(
        connection,
        "_uqa_mvcc_native_expected",
        "old_key IS NULL",
        control,
        |family, key| {
            if physical::get(connection, family.layout(), key, control)?.is_some() {
                return Err(invalid("native target is already owned by a different record").into());
            }
            Ok(())
        },
    )?;
    validate_retired_owners(connection, prepared, control)?;
    // Release changed name bindings before installing any new ones so identity reuse does not depend on lexical name order.
    for family in ORDER.into_iter().rev() {
        let filter = format!(
            "family = {} AND old_key IS NOT NULL AND (old_key IS NOT new_key OR family = {})",
            family.id(),
            Family::TableOwners.id(),
        );
        queue::visit(
            connection,
            "_uqa_mvcc_native_expected",
            &filter,
            control,
            |family, key| physical::remove(connection, family.layout(), key, control),
        )?;
    }
    for family in ORDER {
        for record in prepared.records() {
            if NativeRecordIdentity::decode(record.key())?.family() != family {
                continue;
            }
            let Some(row) = record.value() else { continue };
            let (_, values) = decode_record(record.key(), row, control)?;
            publish_row(connection, family, &values, control)?;
        }
    }
    super::graph_lookup::validate_deletions(connection, prepared, control)?;
    for record in prepared.records() {
        if let Some(row) = record.value() {
            let (identity, values) = decode_record(record.key(), row, control)?;
            owners::validate(connection, database, identity, &values, control)?;
        }
    }
    queue::visit(
        connection,
        "_uqa_mvcc_native_changes",
        "1",
        control,
        |family, key| {
            let row = physical::get(connection, family.layout(), key, control)?;
            if family == Family::CacheRevisions {
                let row =
                    row.ok_or_else(|| invalid("native trigger deleted a cache generation"))?;
                let values = decode_row(&row, family.layout().columns.len(), control)?;
                let record = NativeRecord::encode(
                    family,
                    NativeRecordOwner::Database(database),
                    &values,
                    control,
                )?;
                write::stage_record(
                    connection,
                    record.key(),
                    Some(record.row()),
                    sequence,
                    control,
                )
            } else {
                verify_expected(connection, family, key, row.as_deref(), control)
            }
        },
    )?;
    queue::visit(
        connection,
        "_uqa_mvcc_native_expected",
        "1",
        control,
        |family, key| {
            let row = physical::get(connection, family.layout(), key, control)?;
            verify_expected(connection, family, key, row.as_deref(), control)
        },
    )?;
    connection.execute_batch(
        "DELETE FROM _uqa_mvcc_native_expected; DELETE FROM _uqa_mvcc_native_changes;",
    )?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

fn seed_originals(
    connection: &Connection,
    database: DatabaseId,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for record in prepared.records() {
        let identity = NativeRecordIdentity::decode_full(record.key(), control)?;
        if matches!(identity.owner(), NativeRecordOwner::Database(id) if id != database) {
            return Err(VersionError::WrongDatabase.into());
        }
        if identity.family() == Family::CacheRevisions {
            return Err(
                invalid("native cache generations are provider-owned commit effects").into(),
            );
        }
        read::value(
            connection,
            record.key(),
            CommitSequence::from_u64(u64::MAX),
            control,
            &mut |previous| {
                let validate = || -> PhysicalResult<()> {
                    let Some(row) = previous.and_then(|record| record.value) else {
                        return Ok(());
                    };
                    let (identity, values) = decode_record(record.key(), row, control)?;
                    let family = identity.family();
                    if family == Family::StandaloneGraphScopes && record.value() != Some(row) {
                        return Err(
                            invalid("standalone graph namespace bindings are immutable").into()
                        );
                    }
                    owners::validate(connection, database, identity, &values, control)?;
                    let key = physical::physical_key(family.layout(), &values, control)?;
                    if physical::get(connection, family.layout(), &key, control)?.as_deref()
                        != Some(row)
                    {
                        return Err(invalid(
                            "native materialization disagrees with its record history",
                        )
                        .into());
                    }
                    let _bindings =
                        crate::read_control::reserve_bindings(control, &[&key, record.key()])?;
                    connection.execute("INSERT INTO _uqa_mvcc_native_expected(family, physical_key, old_key) VALUES (?1, ?2, ?3)", params![family.id(), &key[..], record.key()])?;
                    if family == Family::Metadata
                        && values[0] == ValueRef::Text(b"schema_version")
                        && record.value() != Some(row)
                    {
                        return Err(invalid(
                            "native format version cannot be changed by a record commit",
                        )
                        .into());
                    }
                    Ok(())
                };
                validate().map_err(Error::into_version)
            },
        )?;
    }
    Ok(())
}

fn seed_targets(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for record in prepared.records() {
        let Some(row) = record.value() else { continue };
        let (identity, values) = decode_record(record.key(), row, control)?;
        let family = identity.family();
        if family == Family::StandaloneGraphScopes {
            super::standalone_graph::validate_target(connection, &values, control)?;
        }
        if family == Family::Metadata
            && values[0] == ValueRef::Text(b"schema_version")
            && values[1] != ValueRef::Text(b"49")
        {
            return Err(
                invalid("native format version cannot be changed by a record commit").into(),
            );
        }
        let key = physical::physical_key(family.layout(), &values, control)?;
        let _bindings = crate::read_control::reserve_bindings(control, &[&key, record.key(), row])?;
        let changed = connection.execute("INSERT INTO _uqa_mvcc_native_expected(family, physical_key, new_key, new_value) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(family, physical_key) DO UPDATE SET new_key = excluded.new_key, new_value = excluded.new_value WHERE _uqa_mvcc_native_expected.new_key IS NULL", params![family.id(), &key[..], record.key(), row])?;
        if changed != 1 {
            return Err(invalid("two native records claim the same physical primary key").into());
        }
    }
    Ok(())
}

fn validate_retired_owners(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for record in prepared.records() {
        if NativeRecordIdentity::decode(record.key())?.family() != Family::TableOwners {
            continue;
        }
        read::value(
            connection,
            record.key(),
            CommitSequence::from_u64(u64::MAX),
            control,
            &mut |previous| {
                let validate = || -> PhysicalResult<()> {
                    let Some(old) = previous.and_then(|record| record.value) else {
                        return Ok(());
                    };
                    if record.value() == Some(old) {
                        return Ok(());
                    }
                    let values =
                        decode_row(old, Family::TableOwners.layout().columns.len(), control)?;
                    if let Some(new) = record.value() {
                        let updated =
                            decode_row(new, Family::TableOwners.layout().columns.len(), control)?;
                        // Changing catalog membership preserves this owner and needs no data rewrite.
                        if values[..3] == updated[..3] {
                            return Ok(());
                        }
                    }
                    owners::validate_retirement(connection, values[0], control)
                };
                validate().map_err(Error::into_version)
            },
        )?;
    }
    Ok(())
}

fn verify_expected(
    connection: &Connection,
    family: Family,
    key: &[u8],
    row: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let _bindings =
        crate::read_control::reserve_bindings(control, &[key, row.unwrap_or_default()])?;
    let valid: Option<bool> = connection.query_row("SELECT new_value IS ?3 FROM _uqa_mvcc_native_expected WHERE family = ?1 AND physical_key = ?2", params![family.id(), key, row], |row| row.get(0)).optional()?;
    if valid != Some(true) {
        return Err(invalid("native trigger or cascade changed an unprepared record").into());
    }
    Ok(())
}

fn publish_row(
    connection: &Connection,
    family: Family,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if family == Family::StandaloneGraphScopes {
        super::standalone_graph::publish_scope(connection, values, control)
    } else {
        physical::upsert(connection, family.layout(), values, control)
    }
}
