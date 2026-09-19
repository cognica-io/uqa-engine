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

const PREVIOUS_HEADS: &str = "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000')) WITHOUT ROWID";

const TABLES: [(&str, &str); 6] = [
    ("_uqa_mvcc_metadata", "CREATE TABLE _uqa_mvcc_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 33), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), allocated BLOB NOT NULL CHECK(typeof(allocated) = 'blob' AND length(allocated) = 8), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8), mapping INTEGER NOT NULL DEFAULT 0 CHECK(mapping IN (0, 1)))"),
    ("_uqa_mvcc_heads", "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), compacted INTEGER NOT NULL DEFAULT 0 CHECK(compacted IN (0, 1))) WITHOUT ROWID"),
    ("_uqa_mvcc_versions", "CREATE TABLE _uqa_mvcc_versions (key BLOB NOT NULL CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), value BLOB CHECK(value IS NULL OR typeof(value) = 'blob'), PRIMARY KEY(key, sequence)) WITHOUT ROWID"),
    ("_uqa_mvcc_transactions", "CREATE TABLE _uqa_mvcc_transactions (allocation BLOB PRIMARY KEY CHECK(typeof(allocation) = 'blob' AND length(allocation) = 8 AND allocation > x'0000000000000000'), status INTEGER NOT NULL CHECK(status IN (0, 1, 2)), sequence BLOB, fingerprint BLOB, CHECK((status IN (0, 1) AND sequence IS NULL AND fingerprint IS NULL) OR (status = 2 AND typeof(sequence) = 'blob' AND length(sequence) = 8 AND typeof(fingerprint) = 'blob' AND length(fingerprint) = 32))) WITHOUT ROWID"),
    super::identifiers::TABLE,
    super::runs::TABLE,
];

/// A connection-local admission token. Dropping it closes permission without any fallible SQL cleanup, including on unwind or commit failure.
pub(crate) struct WritePermit(Arc<AtomicBool>);

impl WritePermit {
    pub(crate) fn for_native_restore(connection: &Connection) -> super::VersionResult<Self> {
        Self::acquire(connection).map_err(super::Error::into_version)
    }

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
    let initialized = initialize_in(&transaction)?;
    if initialized.created || initialized.upgraded {
        transaction.commit()?;
    }
    Ok(initialized.identity)
}

pub(super) struct Initialization {
    pub(super) identity: DatabaseId,
    pub(super) created: bool,
    pub(super) upgraded: bool,
}

pub(super) fn initialize_in(transaction: &Connection) -> PhysicalResult<Initialization> {
    let (present, predecessor) = validate_tables(transaction)?;
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
            "INSERT INTO _uqa_mvcc_metadata (singleton, format, database_id, allocated, sequence) VALUES (1, 33, ?1, ?2, ?2)",
            params![identity.as_slice(), 0_u64.to_be_bytes().as_slice()],
        )?;
        return Ok(Initialization {
            identity: DatabaseId::from_bytes(identity),
            created: true,
            upgraded: false,
        });
    }
    for (name, _) in TABLES {
        if (predecessor.is_some_and(|format| format < 5) && name == super::identifiers::TABLE.0)
            || (predecessor.is_some_and(|format| format < 29) && name == super::runs::TABLE.0)
        {
            continue;
        }
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let (name, expected) = trigger(name, action);
            if definition_matches(transaction, &name, &expected)? != Some(true) {
                return Err(
                    VersionError::InvalidEncoding("missing or changed record write guard").into(),
                );
            }
        }
    }
    let upgraded = predecessor.is_some();
    if let Some(format) = predecessor {
        upgrade_metadata(transaction, format)?;
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
    Ok(Initialization {
        identity,
        created: false,
        upgraded,
    })
}

fn validate_tables(transaction: &Connection) -> PhysicalResult<(usize, Option<i64>)> {
    let mut present = 0;
    let mut predecessor = None;
    for (name, expected) in TABLES {
        if let Some(matches) = definition_matches(transaction, name, expected)? {
            if !matches {
                if name == TABLES[0].0 {
                    for format in 1..=32 {
                        if definition_matches(
                            transaction,
                            name,
                            &expected.replace(
                                "CHECK(format = 33)",
                                &format!("CHECK(format = {format})"),
                            ),
                        )? == Some(true)
                        {
                            predecessor = Some(format);
                        }
                    }
                }
                let previous_heads = name == TABLES[1].0
                    && predecessor.is_some_and(|format| format < 28)
                    && definition_matches(transaction, name, PREVIOUS_HEADS)? == Some(true);
                if (name != TABLES[0].0 || predecessor.is_none()) && !previous_heads {
                    return Err(VersionError::InvalidEncoding(
                        "unexpected record table definition",
                    )
                    .into());
                }
            }
            present += 1;
        }
    }
    if predecessor.is_some_and(|format| format < 28)
        && definition_matches(transaction, TABLES[1].0, PREVIOUS_HEADS)? != Some(true)
    {
        return Err(VersionError::InvalidEncoding(
            "predecessor has an incompatible record head layout",
        )
        .into());
    }
    let identifier_table = definition_matches(
        transaction,
        super::identifiers::TABLE.0,
        super::identifiers::TABLE.1,
    )?;
    if predecessor.is_some_and(|format| format < 5) && identifier_table.is_some() {
        return Err(VersionError::InvalidEncoding(
            "predecessor contains unexpected identifier allocations",
        )
        .into());
    }
    if predecessor.is_some_and(|format| format < 29)
        && definition_matches(transaction, super::runs::TABLE.0, super::runs::TABLE.1)?.is_some()
    {
        return Err(
            VersionError::InvalidEncoding("predecessor contains unexpected record runs").into(),
        );
    }
    let expected = TABLES.len()
        - usize::from(predecessor.is_some_and(|format| format < 5))
        - usize::from(predecessor.is_some_and(|format| format < 29));
    if present != 0 && present != expected {
        return Err(VersionError::InvalidEncoding("incomplete record table set").into());
    }
    Ok((present, predecessor))
}

fn upgrade_metadata(transaction: &Connection, format: i64) -> PhysicalResult<()> {
    let valid: bool = transaction.query_row(
        "SELECT count(*) = 1 AND coalesce(min(format) = ?1, 0) FROM _uqa_mvcc_metadata",
        [format],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(VersionError::InvalidEncoding("invalid predecessor record format").into());
    }
    if format < 28 {
        transaction.execute_batch("ALTER TABLE _uqa_mvcc_heads ADD COLUMN compacted INTEGER NOT NULL DEFAULT 0 CHECK(compacted IN (0, 1))")?;
    }
    if format < 29 {
        transaction.execute_batch(super::runs::TABLE.1)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&trigger(super::runs::TABLE.0, action).1)?;
        }
    }
    // Existing key/revision identities, histories, receipts and identifier reservations keep their commit boundaries.
    if format < 5 {
        let (name, sql) = super::identifiers::TABLE;
        transaction.execute_batch(sql)?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&trigger(name, action).1)?;
        }
    }
    transaction
        .execute_batch("ALTER TABLE _uqa_mvcc_metadata RENAME TO _uqa_mvcc_previous_metadata")?;
    transaction.execute_batch(TABLES[0].1)?;
    transaction.execute_batch("INSERT INTO _uqa_mvcc_metadata SELECT singleton, 33, database_id, allocated, sequence, mapping FROM _uqa_mvcc_previous_metadata; DROP TABLE _uqa_mvcc_previous_metadata;")?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        transaction.execute_batch(&trigger(TABLES[0].0, action).1)?;
    }
    Ok(())
}
