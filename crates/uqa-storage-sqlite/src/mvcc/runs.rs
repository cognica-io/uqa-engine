//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded, lossless runs of current records with consecutive big-endian key suffixes.

mod collect;
mod scan;

pub(super) use collect::compact;
pub(super) use scan::next_key;

use rusqlite::{params, types::ValueRef, Connection, Row};
use uqa_storage::mvcc::{BorrowedRecord, CommitSequence, RecordValueVisitor, VersionError};
use uqa_storage::read_control::StorageReadControl;

use super::{codec, PhysicalResult};

const MAX_KEY: usize = 1024;
const MAX_VALUE: usize = 128;
const MAX_COUNT: u64 = 128;

pub(super) const TABLE: (&str, &str) = ("_uqa_mvcc_runs", "CREATE TABLE _uqa_mvcc_runs (key_length INTEGER NOT NULL CHECK(key_length BETWEEN 8 AND 1024), first_key BLOB NOT NULL CHECK(typeof(first_key) = 'blob' AND length(first_key) = key_length), last_key BLOB NOT NULL CHECK(typeof(last_key) = 'blob' AND length(last_key) = key_length AND last_key >= first_key), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), kind INTEGER NOT NULL CHECK(kind IN (0, 1, 2)), value BLOB, CHECK((kind = 0 AND value IS NULL) OR (kind = 1 AND typeof(value) = 'blob' AND length(value) <= 128) OR (kind = 2 AND typeof(value) = 'blob' AND length(value) BETWEEN 8 AND 128)), PRIMARY KEY(key_length, first_key)) WITHOUT ROWID");

const LOOKUP: &str = "SELECT first_key, last_key, sequence, kind, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END, key_length FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key <= ?2 ORDER BY first_key DESC LIMIT 1";

struct Bounds {
    first: [u8; MAX_KEY],
    last: [u8; MAX_KEY],
    key_length: usize,
    sequence: CommitSequence,
    kind: i64,
    value_length: Option<usize>,
}

impl Bounds {
    fn decode(row: &Row<'_>) -> PhysicalResult<Self> {
        let first = codec::bytes(row, 0)?;
        let last = codec::bytes(row, 1)?;
        let sequence = codec::integer(codec::bytes(row, 2)?)?;
        let kind = row.get(3)?;
        let length: Option<i64> = row.get(4)?;
        if !(8..=MAX_KEY).contains(&first.len())
            || row.get::<_, i64>(5)? != i64::try_from(first.len()).expect("bounded key length")
            || last.len() != first.len()
            || first[..first.len() - 8] != last[..last.len() - 8]
            || first > last
            || suffix(last) - suffix(first) >= MAX_COUNT
            || sequence == 0
            || !matches!(
                (kind, length),
                (0, None) | (1, Some(0..=128)) | (2, Some(8..=128))
            )
        {
            return Err(VersionError::InvalidEncoding("invalid compacted record run").into());
        }
        let mut bounds = Self {
            first: [0; MAX_KEY],
            last: [0; MAX_KEY],
            key_length: first.len(),
            sequence: CommitSequence::from_u64(sequence),
            kind,
            value_length: length.map(|length| usize::try_from(length).expect("validated length")),
        };
        bounds.first[..first.len()].copy_from_slice(first);
        bounds.last[..last.len()].copy_from_slice(last);
        Ok(bounds)
    }

    fn first(&self) -> &[u8] {
        &self.first[..self.key_length]
    }

    fn last(&self) -> &[u8] {
        &self.last[..self.key_length]
    }

    fn contains(&self, key: &[u8]) -> bool {
        key.len() == self.key_length && self.first() <= key && key <= self.last()
    }
}

fn suffix(key: &[u8]) -> u64 {
    u64::from_be_bytes(key[key.len() - 8..].try_into().expect("eight-byte suffix"))
}

fn bounds(connection: &Connection, key: &[u8]) -> PhysicalResult<Option<Bounds>> {
    if !(8..=MAX_KEY).contains(&key.len()) {
        return Ok(None);
    }
    let mut statement = connection.prepare(LOOKUP)?;
    let mut rows = statement.query(params![
        i64::try_from(key.len()).expect("bounded key length"),
        key
    ])?;
    rows.next()?
        .map(Bounds::decode)
        .transpose()
        .map(|run| run.filter(|run| run.contains(key)))
}

pub(super) fn info(
    connection: &Connection,
    key: &[u8],
) -> PhysicalResult<Option<(CommitSequence, Option<usize>)>> {
    Ok(bounds(connection, key)?.map(|run| (run.sequence, run.value_length)))
}

struct Run {
    bounds: Bounds,
    template: [u8; MAX_VALUE],
}

impl Run {
    fn load(connection: &Connection, bounds: Bounds) -> PhysicalResult<Self> {
        let mut template = [0; MAX_VALUE];
        let mut statement = connection
            .prepare("SELECT value FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key = ?2")?;
        let mut rows = statement.query(params![
            i64::try_from(bounds.key_length).expect("bounded key length"),
            bounds.first()
        ])?;
        let row = rows
            .next()?
            .ok_or(VersionError::InvalidEncoding("record run disappeared"))?;
        match (row.get_ref(0)?, bounds.value_length) {
            (ValueRef::Null, None) => (),
            (ValueRef::Blob(value), Some(length)) if value.len() == length => {
                template[..length].copy_from_slice(value);
            }
            _ => return Err(VersionError::InvalidEncoding("invalid record run template").into()),
        }
        Ok(Self { bounds, template })
    }

    fn value<'a>(&self, key: &[u8], output: &'a mut [u8; MAX_VALUE]) -> Option<&'a [u8]> {
        let length = self.bounds.value_length?;
        output[..length].copy_from_slice(&self.template[..length]);
        if self.bounds.kind == 2 {
            let decoded = suffix(key) ^ suffix(&self.template[..length]);
            output[length - 8..length].copy_from_slice(&decoded.to_be_bytes());
        }
        Some(&output[..length])
    }

    fn insert(&self, connection: &Connection, first: &[u8], last: &[u8]) -> PhysicalResult<()> {
        connection.execute("INSERT INTO _uqa_mvcc_runs (key_length, first_key, last_key, sequence, kind, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![i64::try_from(first.len()).expect("bounded key length"), first, last, self.bounds.sequence.as_u64().to_be_bytes().as_slice(), self.bounds.kind, self.bounds.value_length.map(|length| &self.template[..length])])?;
        Ok(())
    }
}

pub(super) fn value(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    let bounds = bounds(connection, key)?.ok_or(VersionError::InvalidEncoding(
        "record run disappeared within a read",
    ))?;
    if bounds.sequence > boundary {
        return Err(VersionError::InvalidEncoding("record run changed within a read").into());
    }
    let _payload = control
        .memory()
        .reserve(bounds.value_length.unwrap_or(0))
        .map_err(VersionError::from)?;
    let run = Run::load(connection, bounds)?;
    let mut output = [0; MAX_VALUE];
    control.cancellation().check().map_err(VersionError::from)?;
    visit(Some(BorrowedRecord {
        revision: Some(run.bounds.sequence),
        value: run.value(key, &mut output),
    }))?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

/// Restore a single predecessor before replacing it, retaining every other member of its run.
pub(super) fn extract(
    connection: &Connection,
    key: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let Some(bounds) = bounds(connection, key)? else {
        return Ok(());
    };
    let _payload = control
        .memory()
        .reserve(bounds.value_length.unwrap_or(0))
        .map_err(VersionError::from)?;
    let run = Run::load(connection, bounds)?;
    let _bindings = crate::read_control::reserve_bindings(
        control,
        &[
            run.bounds.first(),
            run.bounds.last(),
            &run.template[..run.bounds.value_length.unwrap_or(0)],
        ],
    )?;
    let mut output = [0; MAX_VALUE];
    connection.execute(
        "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, ?3)",
        params![
            key,
            run.bounds.sequence.as_u64().to_be_bytes().as_slice(),
            run.value(key, &mut output)
        ],
    )?;
    connection.execute(
        "DELETE FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key = ?2",
        params![
            i64::try_from(key.len()).expect("bounded key length"),
            run.bounds.first()
        ],
    )?;
    let offset = key.len() - 8;
    let mut split = [0; MAX_KEY];
    split[..key.len()].copy_from_slice(key);
    if key > run.bounds.first() {
        split[offset..key.len()].copy_from_slice(&(suffix(key) - 1).to_be_bytes());
        run.insert(connection, run.bounds.first(), &split[..key.len()])?;
    }
    if key < run.bounds.last() {
        split[offset..key.len()].copy_from_slice(&(suffix(key) + 1).to_be_bytes());
        run.insert(connection, &split[..key.len()], run.bounds.last())?;
    }
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}
