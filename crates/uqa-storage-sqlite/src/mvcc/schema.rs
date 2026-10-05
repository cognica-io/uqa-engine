//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use uqa_storage::mvcc::{DatabaseId, VersionError};

use super::{codec, connection_functions::ConnectionFunctions, PhysicalResult};

/// Writers preserve relation row-type array identities and names.
pub(super) const RECORD_FORMAT: i64 = 56;
const VERSION_METADATA_FORMAT: i64 = 55;

const PREVIOUS_METADATA: &str = "CREATE TABLE _uqa_mvcc_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 42), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), allocated BLOB NOT NULL CHECK(typeof(allocated) = 'blob' AND length(allocated) = 8), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8), mapping INTEGER NOT NULL DEFAULT 0 CHECK(mapping IN (0, 1)))";

const PREVIOUS_RESTORE_METADATA: &str = "CREATE TABLE _uqa_mvcc_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 43), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), allocated BLOB NOT NULL CHECK(typeof(allocated) = 'blob' AND length(allocated) = 8), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8), mapping INTEGER NOT NULL DEFAULT 0 CHECK(mapping IN (0, 1)), restore_target BLOB CHECK(restore_target IS NULL OR (typeof(restore_target) = 'blob' AND length(restore_target) = 16 AND restore_target != database_id)))";

pub(super) const PREVIOUS_TRANSACTIONS: &str = "CREATE TABLE _uqa_mvcc_transactions (allocation BLOB PRIMARY KEY CHECK(typeof(allocation) = 'blob' AND length(allocation) = 8 AND allocation > x'0000000000000000'), status INTEGER NOT NULL CHECK(status IN (0, 1, 2)), sequence BLOB, fingerprint BLOB, CHECK((status IN (0, 1) AND sequence IS NULL AND fingerprint IS NULL) OR (status = 2 AND typeof(sequence) = 'blob' AND length(sequence) = 8 AND typeof(fingerprint) = 'blob' AND length(fingerprint) = 32))) WITHOUT ROWID";

const PREVIOUS_HEADS: &str = "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000')) WITHOUT ROWID";

const TABLES: [(&str, &str); 7] = [
    ("_uqa_mvcc_metadata", "CREATE TABLE _uqa_mvcc_metadata (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 56), database_id BLOB NOT NULL CHECK(typeof(database_id) = 'blob' AND length(database_id) = 16), allocated BLOB NOT NULL CHECK(typeof(allocated) = 'blob' AND length(allocated) = 8), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8), mapping INTEGER NOT NULL DEFAULT 0 CHECK(mapping IN (0, 1)), restore_target BLOB CHECK(restore_target IS NULL OR (typeof(restore_target) = 'blob' AND length(restore_target) = 16 AND restore_target != database_id)), receipt_limit INTEGER NOT NULL DEFAULT 65536 CHECK(receipt_limit > 0))"),
    ("_uqa_mvcc_heads", "CREATE TABLE _uqa_mvcc_heads (key BLOB PRIMARY KEY CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), compacted INTEGER NOT NULL DEFAULT 0 CHECK(compacted IN (0, 1))) WITHOUT ROWID"),
    ("_uqa_mvcc_versions", "CREATE TABLE _uqa_mvcc_versions (key BLOB NOT NULL CHECK(typeof(key) = 'blob'), sequence BLOB NOT NULL CHECK(typeof(sequence) = 'blob' AND length(sequence) = 8 AND sequence > x'0000000000000000'), value BLOB CHECK(value IS NULL OR typeof(value) = 'blob'), PRIMARY KEY(key, sequence)) WITHOUT ROWID"),
    ("_uqa_mvcc_transactions", "CREATE TABLE _uqa_mvcc_transactions (allocation BLOB PRIMARY KEY CHECK(typeof(allocation) = 'blob' AND length(allocation) = 8 AND allocation > x'0000000000000000'), status INTEGER NOT NULL CHECK(status IN (0, 1, 2, 3, 4)), sequence BLOB, fingerprint BLOB, managed INTEGER NOT NULL DEFAULT 0 CHECK(managed IN (0, 1)), CHECK((status IN (0, 1, 3) AND sequence IS NULL AND fingerprint IS NULL) OR (status IN (2, 4) AND typeof(sequence) = 'blob' AND length(sequence) = 8 AND typeof(fingerprint) = 'blob' AND length(fingerprint) = 32))) WITHOUT ROWID"),
    super::identifiers::TABLE,
    super::runs::TABLE,
    super::version_metadata::TABLE,
];

/// A connection-local admission token. Dropping it closes permission without any fallible SQL cleanup, including on unwind or commit failure.
pub(crate) struct WritePermit(Arc<ConnectionFunctions>);

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
        let functions = ConnectionFunctions::of(connection)?;
        functions.require_full_synchronization(connection)?;
        functions.open_write_permit();
        Ok(Self(functions))
    }

    pub(super) fn functions(&self) -> &Arc<ConnectionFunctions> {
        &self.0
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        self.0.close_write_permit();
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
        super::version_metadata::create_triggers(transaction)?;
        let mut identity = [0; 16];
        getrandom::fill(&mut identity).map_err(|error| {
            VersionError::Storage(
                crate::SQLiteError::Io(std::io::Error::other(error.to_string())).into(),
            )
        })?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_metadata (singleton, format, database_id, allocated, sequence) VALUES (1, 56, ?1, ?2, ?2)",
            params![identity.as_slice(), 0_u64.to_be_bytes().as_slice()],
        )?;
        return Ok(Initialization {
            identity: DatabaseId::from_bytes(identity),
            created: true,
            upgraded: false,
        });
    }
    validate_guards(transaction, predecessor)?;
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

fn validate_guards(transaction: &Connection, predecessor: Option<i64>) -> PhysicalResult<()> {
    for (name, _) in TABLES {
        if (predecessor.is_some_and(|format| format < 5) && name == super::identifiers::TABLE.0)
            || (predecessor.is_some_and(|format| format < 29) && name == super::runs::TABLE.0)
            || (predecessor.is_some_and(|format| format < VERSION_METADATA_FORMAT)
                && name == super::version_metadata::TABLE.0)
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
    if predecessor.is_none_or(|format| format >= VERSION_METADATA_FORMAT) {
        super::version_metadata::validate_triggers(transaction)?;
    }
    Ok(())
}

fn validate_tables(transaction: &Connection) -> PhysicalResult<(usize, Option<i64>)> {
    let mut present = 0;
    let mut predecessor = None;
    for (name, expected) in TABLES {
        if let Some(matches) = definition_matches(transaction, name, expected)? {
            if !matches {
                if name == TABLES[0].0 {
                    for format in 44..RECORD_FORMAT {
                        if definition_matches(
                            transaction,
                            name,
                            &expected.replace(
                                "CHECK(format = 56)",
                                &format!("CHECK(format = {format})"),
                            ),
                        )? == Some(true)
                        {
                            predecessor = Some(format);
                        }
                    }
                    if definition_matches(transaction, name, PREVIOUS_RESTORE_METADATA)?
                        == Some(true)
                    {
                        predecessor = Some(43);
                    }
                    for format in 1..=42 {
                        if definition_matches(
                            transaction,
                            name,
                            &PREVIOUS_METADATA.replace(
                                "CHECK(format = 42)",
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
                let previous_transactions = name == TABLES[3].0
                    && predecessor.is_some_and(|format| format < 44)
                    && definition_matches(transaction, name, PREVIOUS_TRANSACTIONS)? == Some(true);
                if (name != TABLES[0].0 || predecessor.is_none())
                    && !previous_heads
                    && !previous_transactions
                {
                    return Err(VersionError::InvalidEncoding(
                        "unexpected record table definition",
                    )
                    .into());
                }
            }
            present += 1;
        }
    }
    validate_record_table_set(transaction, present, predecessor)?;
    Ok((present, predecessor))
}

fn validate_record_table_set(
    transaction: &Connection,
    present: usize,
    predecessor: Option<i64>,
) -> PhysicalResult<()> {
    if predecessor.is_some_and(|format| format < 28)
        && definition_matches(transaction, TABLES[1].0, PREVIOUS_HEADS)? != Some(true)
    {
        return Err(VersionError::InvalidEncoding(
            "predecessor has an incompatible record head layout",
        )
        .into());
    }
    if predecessor.is_some_and(|format| format < 44)
        && definition_matches(transaction, TABLES[3].0, PREVIOUS_TRANSACTIONS)? != Some(true)
    {
        return Err(VersionError::InvalidEncoding(
            "predecessor has an incompatible transaction receipt layout",
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
    if predecessor.is_some_and(|format| format < VERSION_METADATA_FORMAT)
        && definition_matches(
            transaction,
            super::version_metadata::TABLE.0,
            super::version_metadata::TABLE.1,
        )?
        .is_some()
    {
        return Err(VersionError::InvalidEncoding(
            "predecessor contains unexpected version metadata",
        )
        .into());
    }
    let expected = TABLES.len()
        - usize::from(predecessor.is_some_and(|format| format < 5))
        - usize::from(predecessor.is_some_and(|format| format < 29))
        - usize::from(predecessor.is_some_and(|format| format < VERSION_METADATA_FORMAT));
    if present != 0 && present != expected {
        return Err(VersionError::InvalidEncoding("incomplete record table set").into());
    }
    Ok(())
}

/// A resumed restore validates and, when necessary, atomically upgrades the intent-bearing predecessor without admitting normal record access through its pending marker.
pub(super) fn initialize_restoration(connection: &Connection) -> PhysicalResult<()> {
    let (present, predecessor) = validate_tables(connection)?;
    if present
        != TABLES.len()
            - usize::from(predecessor.is_some_and(|format| format < VERSION_METADATA_FORMAT))
        || predecessor.is_some_and(|format| !(43..RECORD_FORMAT).contains(&format))
    {
        return Err(VersionError::InvalidEncoding("incomplete restoration record schema").into());
    }
    validate_guards(connection, predecessor)?;
    if let Some(format) = predecessor {
        upgrade_metadata(connection, format)?;
    }
    Ok(())
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
    if format < VERSION_METADATA_FORMAT {
        super::version_metadata::upgrade(transaction)?;
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
    super::identifiers::consolidate_diskann_generations(transaction)?;
    transaction
        .execute_batch("ALTER TABLE _uqa_mvcc_metadata RENAME TO _uqa_mvcc_previous_metadata")?;
    transaction.execute_batch(TABLES[0].1)?;
    let pending = if format >= 43 {
        "restore_target"
    } else {
        "NULL"
    };
    let receipt_limit = if format >= 44 {
        "receipt_limit"
    } else {
        "65536"
    };
    transaction.execute_batch(&format!("INSERT INTO _uqa_mvcc_metadata SELECT singleton, 56, database_id, allocated, sequence, mapping, {pending}, {receipt_limit} FROM _uqa_mvcc_previous_metadata; DROP TABLE _uqa_mvcc_previous_metadata;"))?;
    if format >= 44 {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&trigger(TABLES[0].0, action).1)?;
        }
        return Ok(());
    }
    transaction.execute_batch(
        "ALTER TABLE _uqa_mvcc_transactions RENAME TO _uqa_mvcc_previous_transactions",
    )?;
    transaction.execute_batch(TABLES[3].1)?;
    transaction.execute_batch("INSERT INTO _uqa_mvcc_transactions (allocation, status, sequence, fingerprint) SELECT * FROM _uqa_mvcc_previous_transactions; DROP TABLE _uqa_mvcc_previous_transactions;")?;
    for action in ["INSERT", "UPDATE", "DELETE"] {
        transaction.execute_batch(&trigger(TABLES[3].0, action).1)?;
    }
    for action in ["INSERT", "UPDATE", "DELETE"] {
        transaction.execute_batch(&trigger(TABLES[0].0, action).1)?;
    }
    Ok(())
}
