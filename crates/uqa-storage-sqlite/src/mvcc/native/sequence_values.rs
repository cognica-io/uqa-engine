//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The value state of each sequence definition generation is a record of its own, owned by the generation like its definition. Value operations move it outside the transactions that change the definition, as `PostgreSQL` keeps a sequence's state in the sequence relation's data rather than in its catalog rows, and its changes do not invalidate catalog caches.

use rusqlite::{params, types::ValueRef, Connection};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{CommitSequence, DatabaseId, PreparedRecordCommit, VersionError},
    read_control::StorageReadControl,
};

use super::{
    decode_record, invalid,
    NativeColumnType::{Blob, Integer},
    NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordLayout,
    NativeRecordOwner,
};
use crate::mvcc::{codec, read, write, PhysicalResult};

pub(super) const SQL: &str = "CREATE TABLE _uqa_mvcc_native_sequence_values (object_id BLOB NOT NULL CHECK(typeof(object_id) = 'blob' AND length(object_id) = 16), definition_generation BLOB NOT NULL CHECK(typeof(definition_generation) = 'blob' AND length(definition_generation) = 16), current INTEGER NOT NULL, called INTEGER NOT NULL CHECK(called IN (0, 1)), log_count INTEGER NOT NULL CHECK(log_count >= 0), PRIMARY KEY (object_id, definition_generation)) WITHOUT ROWID";

pub(super) const LAYOUT: NativeRecordLayout = NativeRecordLayout {
    family: Family::SequenceValues,
    table: "_uqa_mvcc_native_sequence_values",
    columns: &[
        "object_id",
        "definition_generation",
        "current",
        "called",
        "log_count",
    ],
    column_types: &[Blob, Blob, Integer, Integer, Integer],
    nullable: &[false; 5],
    primary_key: &[0, 1],
    identity_columns: &[],
    object_owned: true,
};

/// The columns of a sequence definition that hold the value state its generation started with, which the value record of a database converted from a catalog without value records takes over.
const DEFINITION_VALUES: &str = "object_id, definition_generation, current, called, log_count";

/// Fill the value records of a catalog that is converted to native records, from the value state its sequence definitions hold.
pub(super) fn seed(connection: &Connection) -> PhysicalResult<()> {
    connection.execute_batch(&format!(
        "INSERT INTO {} ({DEFINITION_VALUES}) SELECT {DEFINITION_VALUES} FROM _sequences",
        LAYOUT.table
    ))?;
    Ok(())
}

/// Give an existing native history the value record of every sequence definition generation it holds, mirroring each retained revision of the definition, whose value columns held the value state, at the same commit sequence. Readers of any retained snapshot then find the value state they found before. Runs inside the caller's format transaction before the family's capture triggers exist, so the materialization changes are not captured as a commit.
pub(super) fn backfill(
    connection: &Connection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    // A pending restore upgrades the restored history in its owner's transaction, before normal record access is allowed.
    let (identity, header, _) = codec::restoration_header(connection)?;
    if identity != database {
        return Err(VersionError::WrongDatabase.into());
    }
    let boundary = header.sequence;
    let prefix = NativeRecordIdentity::family_prefix(Family::SequenceValues, control)?;
    let mut present = false;
    read::keys(connection, &prefix, None, 1, control, &mut |_| {
        present = true;
        Ok(Some(false))
    })?;
    if present {
        return Err(
            invalid("native sequence value history already exists before migration").into(),
        );
    }
    let definitions = NativeRecordIdentity::family_prefix(Family::Sequences, control)?;
    read::keys(
        connection,
        &definitions,
        None,
        usize::MAX,
        control,
        &mut |key| {
            backfill_definition(connection, key, boundary, control)?;
            Ok(Some(true))
        },
    )?;
    seed(connection)
}

fn backfill_definition(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let identity = NativeRecordIdentity::decode_full(key, control)?;
    let owner = identity.owner();
    if !matches!(owner, NativeRecordOwner::Object { .. }) {
        return Err(invalid("native sequence definition is not object owned").into());
    }
    let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
    // Every retained revision of the definition becomes an ordinary version row; reclaimed revisions, and a reclaimed deletion, have no counterpart, as readers of a retained snapshot see none of them.
    crate::mvcc::runs::extract(connection, key, control)?;
    let mut statement = connection.prepare(
        "SELECT sequence FROM _uqa_mvcc_version_metadata WHERE key = ?1 ORDER BY sequence",
    )?;
    let mut versions = statement.query(params![key])?;
    while let Some(row) = versions.next()? {
        control.cancellation().check().map_err(VersionError::from)?;
        let sequence = CommitSequence::from_u64(codec::integer(codec::bytes(row, 0)?)?);
        if sequence.as_u64() == 0 || sequence > boundary {
            return Err(
                invalid("sequence definition history has an invalid commit boundary").into(),
            );
        }
        let mut value: Option<NativeRecord> = None;
        read::value(connection, key, sequence, control, &mut |record| {
            if let Some(bytes) = record.and_then(|record| record.value) {
                let (_, definition) = decode_record(key, bytes, control)?;
                value = Some(record_of(owner, &definition, control)?);
            }
            Ok(())
        })?;
        let value_key =
            NativeRecordIdentity::new(Family::SequenceValues, owner)?.encode_key(&[], control)?;
        write::stage_record(
            connection,
            &value_key,
            value.as_ref().map(NativeRecord::row),
            sequence,
            control,
        )?;
    }
    Ok(())
}

/// The value record of a definition generation, holding the value columns of one of its definition's revisions.
fn record_of(
    owner: NativeRecordOwner,
    definition: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> Result<NativeRecord, VersionError> {
    let NativeRecordOwner::Object {
        identity,
        generation,
    } = owner
    else {
        return Err(invalid("native sequence definition is not object owned"));
    };
    let mut values = BudgetedVec::new(control.memory());
    values.extend_from_slice(&[
        ValueRef::Blob(&identity),
        ValueRef::Blob(&generation),
        definition[5],
        definition[6],
        definition[20],
    ])?;
    NativeRecord::encode(Family::SequenceValues, owner, &values, control)
}

/// Every definition generation a commit leaves has its value record, and every value record its definition, so that no reader finds one without the other.
pub(super) fn validate_prepared(
    connection: &Connection,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let mut touched = BudgetedVec::new(control.memory());
    let mut records = prepared.writes();
    while let Some(record) = records.next_metadata(control)? {
        let identity = NativeRecordIdentity::decode(record.key())?;
        let definition = match identity.family() {
            Family::Sequences => true,
            Family::SequenceValues => false,
            _ => continue,
        };
        let NativeRecordOwner::Object {
            identity,
            generation,
        } = identity.owner()
        else {
            return Err(invalid("native sequence records are object owned").into());
        };
        touched
            .push((identity, generation, definition, record.live()))
            .map_err(VersionError::from)?;
    }
    touched.sort_unstable();
    let mut index = 0;
    while index < touched.len() {
        let (identity, generation, ..) = touched[index];
        // Indexed by whether the record is the definition.
        let mut live = [None, None];
        while index < touched.len()
            && (touched[index].0, touched[index].1) == (identity, generation)
        {
            live[usize::from(touched[index].2)] = Some(touched[index].3);
            index += 1;
        }
        let owner = NativeRecordOwner::Object {
            identity,
            generation,
        };
        let mut resolved = [false; 2];
        for (slot, family) in [(0, Family::SequenceValues), (1, Family::Sequences)] {
            resolved[slot] = match live[slot] {
                Some(live) => live,
                None => committed_live(connection, family, owner, control)?,
            };
        }
        if resolved[0] != resolved[1] {
            return Err(invalid(
                "a native sequence definition generation and its value record must change together",
            )
            .into());
        }
    }
    Ok(())
}

fn committed_live(
    connection: &Connection,
    family: Family,
    owner: NativeRecordOwner,
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    let key = NativeRecordIdentity::new(family, owner)?.encode_key(&[], control)?;
    let _bindings = crate::read_control::reserve_bindings(control, &[&key])?;
    Ok(
        read::info(connection, &key, CommitSequence::from_u64(u64::MAX))?
            .is_some_and(|info| info.length.is_some()),
    )
}
