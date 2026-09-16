//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Add graph selectors to an existing native history in the caller's one atomic format transaction.

use rusqlite::Connection;
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId},
    read_control::StorageReadControl,
};

use super::{graph_lookup, install_family_guards, invalid, physical, schema, Family, TABLES};
use crate::mvcc::{
    native::{decode_record, NativeRecord, NativeRecordOwner},
    read, PhysicalResult,
};

pub(super) fn upgrade(
    connection: &Connection,
    database: DatabaseId,
    version: u32,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let sources = if version == 1 {
        &graph_lookup::SOURCES[..]
    } else {
        &[Family::GraphPathIndexState]
    };
    validate_sources(connection, database, sources, control)?;
    if version == 1 {
        connection.execute_batch(graph_lookup::SQL)?;
    }
    graph_lookup::backfill(connection, database, sources, version == 2, control)?;
    // Backfill supplies the complete history itself. Suppress ordinary commit capture only while installing the new current projections, inside this same schema transaction.
    if version == 2 {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, _) = super::capture::trigger(Family::GraphLookups, action);
            connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
    }
    graph_lookup::seed_sources(connection, sources, control)?;
    if version == 2 {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            connection.execute_batch(&super::capture::trigger(Family::GraphLookups, action).1)?;
        }
    }
    connection.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
    connection.execute_batch(TABLES[0].1)?;
    connection.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1, 3, 49)", [])?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        connection.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
    }
    if version == 1 {
        install_family_guards(connection, Family::GraphLookups)?;
    }
    for (_, sql) in graph_lookup::source_triggers(sources) {
        connection.execute_batch(&sql)?;
    }
    super::validate_format(connection, 3)?;
    super::check_mapping_version(connection, 3)
}

fn validate_sources(
    connection: &Connection,
    database: DatabaseId,
    sources: &[Family],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for &family in sources {
        physical::visit(connection, family.layout(), control, |values| {
            let encoded = NativeRecord::encode(
                family,
                NativeRecordOwner::Database(database),
                values,
                control,
            )?;
            read::value(
                connection,
                encoded.key(),
                CommitSequence::from_u64(u64::MAX),
                control,
                &mut |record| {
                    let Some(bytes) = record.and_then(|record| record.value) else {
                        return Err(invalid("native graph source has no current record history"));
                    };
                    let (_, stored) = decode_record(encoded.key(), bytes, control)?;
                    if &*stored != values {
                        return Err(invalid(
                            "native graph source disagrees with its current record history",
                        ));
                    }
                    Ok(())
                },
            )
        })?;
    }
    Ok(())
}
