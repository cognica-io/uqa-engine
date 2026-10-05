//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply evaluated native records and verify every trigger/cascade result before publishing their history. Cache generations are provider-owned additive effects, captured at the same commit sequence.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId, PreparedRecordCommit, PreparedRecordWrite, VersionError},
    read_control::StorageReadControl,
};

use super::{
    capture::Capture, decode_record, decode_row, invalid, owners, physical, queue, NativeRecord,
    NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
};
use crate::mvcc::{read, write, Error, PhysicalResult};

// Parents precede their children; native document guards and graph invalidation triggers also run before evaluated index materializations are installed.
const ORDER: [Family; 63] = [
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
    Family::SequenceValues,
    Family::Views,
    Family::ForeignServers,
    Family::ForeignServerMetadata,
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
    Family::DiskANNRecords,
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
    Family::VectorOrigins,
    Family::VectorChanges,
    Family::VectorPopulations,
    Family::VectorPopulationWitnesses,
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
    super::sequence_values::validate_prepared(connection, prepared, control)?;
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
    // Release changed name bindings before installing any new ones so identity reuse does not depend on lexical name order. Most commits replace no physical key, so the families that do are read first.
    let replaced = replaced_families(connection)?;
    for family in ORDER.into_iter().rev() {
        if !replaced.contains(&family.id()) {
            continue;
        }
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
    publish_rows(connection, prepared, control)?;
    super::graph_lookup::validate_deletions(connection, prepared, control)?;
    // Every owner binding of the commit is published by now, so the rows of one table share one read of its binding.
    let mut table_owners = owners::TableOwners::default();
    let mut records = prepared.writes();
    while let Some(record) = records.next(control)? {
        if let Some(row) = record.value() {
            let (identity, values) = decode_record(record.key(), row, control)?;
            owners::validate_among(
                &mut table_owners,
                connection,
                database,
                identity,
                &values,
                control,
            )?;
        }
    }
    verify_changes(connection, database, sequence, control)?;
    connection
        .prepare_cached("DELETE FROM _uqa_mvcc_native_expected")?
        .execute([])?;
    connection
        .prepare_cached("DELETE FROM _uqa_mvcc_native_changes")?
        .execute([])?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

/// Verify what publishing the commit's rows changed, and stage the cache generations those changes produced.
fn verify_changes(
    connection: &Connection,
    database: DatabaseId,
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    // A trigger or cascade may change only rows the commit prepared. The last pass verifies that every prepared row holds its prepared value, so this one only looks for a changed row that was not prepared. Cache generations are provider-owned effects of the changes and are staged at this sequence.
    let unprepared: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_native_changes AS changed WHERE changed.family != ?1 AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_expected AS expected WHERE expected.family = changed.family AND expected.physical_key = changed.physical_key))")?
        .query_row([Family::CacheRevisions.id()], |row| row.get(0))?;
    if unprepared {
        return Err(invalid("native trigger or cascade changed an unprepared record").into());
    }
    queue::visit(
        connection,
        "_uqa_mvcc_native_changes",
        &format!("family = {}", Family::CacheRevisions.id()),
        control,
        |family, key| {
            let row = physical::get(connection, family.layout(), key, control)?
                .ok_or_else(|| invalid("native trigger deleted a cache generation"))?;
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
        },
    )?;
    queue::visit_with_value(
        connection,
        "_uqa_mvcc_native_expected",
        "1",
        "new_value",
        control,
        |family, key, expected| {
            let row = physical::get(connection, family.layout(), key, control)?;
            match expected {
                queue::QueuedValue::Read(expected) if expected == row.as_deref() => Ok(()),
                queue::QueuedValue::Read(_) => {
                    Err(invalid("native trigger or cascade changed an unprepared record").into())
                }
                queue::QueuedValue::Unread => {
                    verify_expected(connection, family, key, row.as_deref(), control)
                }
            }
        },
    )?;
    Ok(())
}

/// The families with a physical key the commit removes before it publishes its rows.
fn replaced_families(connection: &Connection) -> PhysicalResult<Vec<u16>> {
    let mut statement = connection.prepare_cached("SELECT DISTINCT family FROM _uqa_mvcc_native_expected WHERE old_key IS NOT NULL AND (old_key IS NOT new_key OR family = ?1)")?;
    let families = statement
        .query_map([Family::TableOwners.id()], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<u16>>>()?;
    Ok(families)
}

/// The positions of the records of `prepared` in publication order: by family in [`ORDER`], and within a family as prepared. A family outside the order is not published.
/// Publish the rows of the commit's records, families in `ORDER` and each family's rows in the order of its records.
fn publish_rows(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if let Some(records) = prepared.resident() {
        for (_, index) in publication_order(records, control)?.iter() {
            let record = &records[*index];
            let Some(row) = record.value() else { continue };
            let (identity, values) = decode_record(record.key(), row, control)?;
            publish_row(connection, identity.family(), &values, control)?;
        }
        return Ok(());
    }
    // A spilled commit is ordered by key, and the key of a native record begins with its family, so each family is one range of it.
    for family in ORDER {
        let prefix = NativeRecordIdentity::family_prefix(family, control)?;
        let mut records = prepared
            .spilled_writes_from(&prefix)
            .expect("a commit without resident writes is spilled");
        while let Some(record) = records.next(control)? {
            if !record.key().starts_with(&prefix) {
                break;
            }
            let Some(row) = record.value() else { continue };
            let (identity, values) = decode_record(record.key(), row, control)?;
            publish_row(connection, identity.family(), &values, control)?;
        }
    }
    Ok(())
}

fn publication_order(
    records: &[PreparedRecordWrite],
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<(usize, usize)>> {
    let mut ordered = BudgetedVec::new(control.memory());
    for (index, record) in records.iter().enumerate() {
        control.cancellation().check().map_err(VersionError::from)?;
        let family = NativeRecordIdentity::decode(record.key())?.family();
        if let Some(position) = ORDER.iter().position(|ordered| *ordered == family) {
            ordered
                .push((position, index))
                .map_err(VersionError::from)?;
        }
    }
    ordered.sort_unstable();
    Ok(ordered)
}

fn seed_originals(
    connection: &Connection,
    database: DatabaseId,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    // Nothing is published yet, so the rows of one table share one read of its owner binding.
    let mut table_owners = owners::TableOwners::default();
    let mut records = prepared.writes();
    while let Some(record) = records.next(control)? {
        let record = &record;
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
                let mut validate = || -> PhysicalResult<()> {
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
                    owners::validate_among(
                        &mut table_owners,
                        connection,
                        database,
                        identity,
                        &values,
                        control,
                    )?;
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
                    connection.prepare_cached("INSERT INTO _uqa_mvcc_native_expected(family, physical_key, old_key) VALUES (?1, ?2, ?3)")?.execute(params![family.id(), &key[..], record.key()])?;
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
    let mut records = prepared.writes();
    while let Some(record) = records.next(control)? {
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
        let changed = connection.prepare_cached("INSERT INTO _uqa_mvcc_native_expected(family, physical_key, new_key, new_value) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(family, physical_key) DO UPDATE SET new_key = excluded.new_key, new_value = excluded.new_value WHERE _uqa_mvcc_native_expected.new_key IS NULL")?.execute(params![family.id(), &key[..], record.key(), row])?;
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
    let mut records = prepared.writes();
    while let Some(record) = records.next(control)? {
        let record = &record;
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
    let valid: Option<bool> = connection.prepare_cached("SELECT new_value IS ?3 FROM _uqa_mvcc_native_expected WHERE family = ?1 AND physical_key = ?2")?.query_row(params![family.id(), key, row], |row| row.get(0)).optional()?;
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
