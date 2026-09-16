//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use rusqlite::{
    functions::FunctionFlags, params, Connection, OptionalExtension, Transaction,
    TransactionBehavior,
};
use uqa_storage::mvcc::{DatabaseId, VersionError};

use super::{codec, PhysicalResult};

const TABLES: [(&str, &str); 4] = [
    ("_uqa_mvcc_metadata", "CREATE TABLE _uqa_mvcc_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 1), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), allocated BLOB NOT NULL CHECK(typeof(allocated) = 'blob' AND length(allocated) = 8), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8), mapping INTEGER NOT NULL DEFAULT 0 CHECK(mapping IN (0, 1)))"),
    ("_uqa_mvcc_heads", "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000')) WITHOUT ROWID"),
    ("_uqa_mvcc_versions", "CREATE TABLE _uqa_mvcc_versions (key BLOB NOT NULL CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), value BLOB CHECK(value IS NULL OR typeof(value) = 'blob'), PRIMARY KEY(key, sequence)) WITHOUT ROWID"),
    ("_uqa_mvcc_transactions", "CREATE TABLE _uqa_mvcc_transactions (allocation BLOB PRIMARY KEY CHECK(typeof(allocation) = 'blob' AND length(allocation) = 8 AND allocation > x'0000000000000000'), status INTEGER NOT NULL CHECK(status IN (0, 1, 2)), sequence BLOB, fingerprint BLOB, CHECK((status IN (0, 1) AND sequence IS NULL AND fingerprint IS NULL) OR (status = 2 AND typeof(sequence) = 'blob' AND length(sequence) = 8 AND typeof(fingerprint) = 'blob' AND length(fingerprint) = 32))) WITHOUT ROWID"),
];

/// A connection-local admission token. Dropping it closes permission without any fallible SQL cleanup, including on unwind or commit failure.
pub(super) struct WritePermit(Arc<AtomicBool>);

impl WritePermit {
    pub(super) fn acquire(connection: &Connection) -> PhysicalResult<Self> {
        if !connection.is_autocommit() {
            return Err(VersionError::InvalidEncoding(
                "record persistence requires an independent physical transaction",
            )
            .into());
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        let enabled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&enabled);
        connection.create_scalar_function(
            "__uqa_mvcc_write_permit",
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |_| Ok(i64::from(flag.load(Ordering::Acquire))),
        )?;
        enabled.store(true, Ordering::Release);
        Ok(Self(enabled))
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn begin(connection: &Connection) -> PhysicalResult<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        connection,
        TransactionBehavior::Immediate,
    )?)
}

pub(super) fn definition_matches(
    connection: &Connection,
    name: &str,
    expected: &str,
) -> PhysicalResult<Option<bool>> {
    Ok(connection
        .query_row(
            "SELECT sql = ?2 FROM sqlite_schema WHERE name = ?1",
            params![name, expected],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn trigger(table: &str, action: &str) -> (String, String) {
    let name = format!("{table}_{action}_guard");
    let sql = format!("CREATE TRIGGER {name} BEFORE {action} ON {table} WHEN __uqa_mvcc_write_permit() != 1 BEGIN SELECT RAISE(ABORT, 'versioned records require commit admission'); END");
    (name, sql)
}

pub(super) fn initialize(connection: &Connection) -> PhysicalResult<DatabaseId> {
    let _permit = WritePermit::acquire(connection)?;
    let transaction = begin(connection)?;
    super::native::reject_mapped(&transaction)?;
    let (identity, created) = initialize_in(&transaction)?;
    if created {
        transaction.commit()?;
    }
    Ok(identity)
}

pub(super) fn initialize_in(transaction: &Connection) -> PhysicalResult<(DatabaseId, bool)> {
    let mut present = 0;
    for (name, expected) in TABLES {
        if let Some(matches) = definition_matches(transaction, name, expected)? {
            if !matches {
                return Err(
                    VersionError::InvalidEncoding("unexpected record table definition").into(),
                );
            }
            present += 1;
        }
    }
    if present != 0 && present != TABLES.len() {
        return Err(VersionError::InvalidEncoding("incomplete record table set").into());
    }
    if present == 0 {
        for (name, sql) in TABLES {
            transaction.execute_batch(sql)?;
            for action in ["INSERT", "UPDATE", "DELETE"] {
                transaction.execute_batch(&trigger(name, action).1)?;
            }
        }
        let mut identity = [0; 16];
        getrandom::fill(&mut identity).map_err(|error| {
            VersionError::Storage(
                crate::SQLiteError::Io(std::io::Error::other(error.to_string())).into(),
            )
        })?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_metadata (singleton, format, database_id, allocated, sequence) VALUES (1, 1, ?1, ?2, ?2)",
            params![identity.as_slice(), 0_u64.to_be_bytes().as_slice()],
        )?;
        return Ok((DatabaseId::from_bytes(identity), true));
    }
    for (name, _) in TABLES {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, expected) = trigger(name, action);
            if definition_matches(transaction, &name, &expected)? != Some(true) {
                return Err(
                    VersionError::InvalidEncoding("missing or changed record write guard").into(),
                );
            }
        }
    }
    let identity = {
        let mut statement = transaction
            .prepare("SELECT database_id FROM _uqa_mvcc_metadata WHERE singleton = 1")?;
        let mut rows = statement.query([])?;
        let row = rows
            .next()?
            .ok_or(VersionError::InvalidEncoding("missing database identity"))?;
        codec::identity(codec::bytes(row, 0)?)?
    };
    codec::header(transaction, identity)?;
    // A verified existing format does not need another durable write.
    Ok((identity, false))
}
