//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A durable intent bridges main-record and auxiliary-coordinator publication across crash recovery.

mod leases;
#[cfg(test)]
mod tests;

use rusqlite::{params, Connection};
use uqa_storage::{
    mvcc::{DatabaseId, DatabaseRestore, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::{
    admission, codec, key_value, native, schema, Error, PhysicalResult, SQLiteSerializableAdmission,
};
use crate::{ManagedConnection, SQLiteError};

#[derive(PartialEq, Eq)]
struct State {
    identity: DatabaseId,
    pending: bool,
}

/// Normal pool creation must not expose an interrupted restore as a usable physical session.
pub(crate) fn reject_pending(connection: &Connection) -> crate::Result<()> {
    let present: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('_uqa_mvcc_metadata') WHERE name = 'restore_target')",
        [],
        |row| row.get(0),
    )?;
    if present
        && connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_metadata WHERE restore_target IS NOT NULL)",
            [],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(SQLiteError::DatabaseRestoreIncomplete);
    }
    Ok(())
}

pub(crate) fn publish(
    connection: &ManagedConnection,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> VersionResult<()> {
    publish_in(connection, request, control).map_err(Error::into_version)
}

fn publish_in(
    connection: &ManagedConnection,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let main = connection.lease_connection_with_control(control)?;
    let before = preflight(&main, request)?;
    let auxiliary = SQLiteSerializableAdmission::for_restore(
        connection,
        request,
        before.identity,
        before.pending,
        control,
    )?;
    // Legacy binaries predate the physical-owner registry. Their detached snapshots and SSI participants still have to close before the main format can change.
    let _leases = leases::exclude(connection, before.identity, auxiliary.graph(), control)?;
    let _timeout = admission::BusyTimeout::new(&main)?;
    let _permit = admission::permit(&main, control)?;
    let transaction = admission::begin(&main, control)?;
    if preflight(&transaction, request)? != before {
        return Err(VersionError::WrongDatabase.into());
    }
    validate_mapping(&transaction, &before, control)?;
    if !request.needs_restore(before.identity)? {
        auxiliary
            .graph()
            .validate_persisted_publications(control, |id| {
                if id.database() != before.identity {
                    return Err(VersionError::WrongDatabase);
                }
                codec::status(&transaction, id).map_err(Error::into_version)
            })?;
        admission::commit(transaction, control)?;
        auxiliary.persist(control)?;
        return Ok(());
    }
    if !before.pending {
        transaction.execute(
            "UPDATE _uqa_mvcc_metadata SET restore_target = ?1 WHERE singleton = 1",
            params![request.target().as_bytes()],
        )?;
    }
    admission::commit(transaction, control)?;
    #[cfg(test)]
    tests::boundary(tests::Boundary::IntentPublished)?;

    auxiliary.publish_restored(request, control)?;
    #[cfg(test)]
    tests::boundary(tests::Boundary::CoordinatorPublished)?;

    let transaction = admission::begin(&main, control)?;
    let (identity, _, pending) = codec::restoration_header(&transaction)?;
    if identity != request.source() || pending != Some(request.target()) {
        return Err(VersionError::InvalidRestoreIdentity.into());
    }
    // Only transaction-history identity changes. Existing revisions, data namespaces and allocation watermarks retain their exact values.
    transaction.execute_batch("DELETE FROM _uqa_mvcc_transactions")?;
    transaction.execute(
        "UPDATE _uqa_mvcc_metadata SET database_id = ?1, restore_target = NULL WHERE singleton = 1",
        params![request.target().as_bytes()],
    )?;
    admission::commit(transaction, control)?;
    #[cfg(test)]
    tests::boundary(tests::Boundary::Completed)?;
    Ok(())
}

fn preflight(connection: &Connection, request: DatabaseRestore) -> PhysicalResult<State> {
    let (format, identity): (i64, [u8; 16]) = connection.query_row(
        "SELECT format, database_id FROM _uqa_mvcc_metadata WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let identity = DatabaseId::from_bytes(identity);
    let source = request.needs_restore(identity)?;
    let pending = match format {
        1..=42 => None,
        43..=45 => connection
            .query_row(
                "SELECT restore_target FROM _uqa_mvcc_metadata WHERE singleton = 1",
                [],
                |row| row.get::<_, Option<[u8; 16]>>(0),
            )?
            .map(DatabaseId::from_bytes),
        46 => codec::restoration_header(connection)?.2,
        _ => return Err(VersionError::InvalidEncoding("unknown record format").into()),
    };
    if pending.is_some_and(|target| !source || target != request.target()) {
        return Err(VersionError::InvalidRestoreIdentity.into());
    }
    Ok(State {
        identity,
        pending: pending.is_some(),
    })
}

fn validate_mapping(
    connection: &Connection,
    state: &State,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    if state.pending {
        schema::initialize_restoration(connection)?;
    } else if native::present(connection)? {
        native::initialize_in(connection, control)?;
    } else {
        schema::initialize_in(connection)?;
    }
    let (identity, header, _) = codec::restoration_header(connection)?;
    if identity != state.identity {
        return Err(VersionError::WrongDatabase.into());
    }
    if native::present(connection)? {
        if header.key_value_mapping {
            return Err(VersionError::InvalidEncoding("mixed native and KeyValue mapping").into());
        }
        native::validate_restoration(connection)?;
    } else if header.key_value_mapping {
        key_value::validate_mapping(connection)?;
    }
    Ok(())
}
