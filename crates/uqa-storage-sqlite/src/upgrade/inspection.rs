//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use uqa_storage::mvcc::VersionError;

use super::StorageUpgradeStep;
use crate::{
    compressed_vfs, detect_database_file_format, DatabaseFileFormat, Result,
    SQLiteCompressionOptions, SQLiteError, SQLiteRecordStore,
};

/// What an open of a database file would do to its native mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageUpgradeInspection {
    stored_mapping: Option<u32>,
    current_mapping: u32,
    steps: Vec<(StorageUpgradeStep, Option<u64>)>,
}

impl StorageUpgradeInspection {
    pub(crate) const fn new(
        stored_mapping: Option<u32>,
        current_mapping: u32,
        steps: Vec<(StorageUpgradeStep, Option<u64>)>,
    ) -> Self {
        Self {
            stored_mapping,
            current_mapping,
            steps,
        }
    }

    /// The mapping the file is stored in.
    ///
    /// `None` for a file that is missing or holds no native mapping. An open
    /// creates the mapping of such a file, or converts a catalog of an
    /// earlier kind into it, which this inspection does not size.
    #[must_use]
    pub const fn stored_mapping(&self) -> Option<u32> {
        self.stored_mapping
    }

    /// The mapping this Engine reads and writes.
    #[must_use]
    pub const fn current_mapping(&self) -> u32 {
        self.current_mapping
    }

    /// Whether an open with this Engine upgrades the file.
    #[must_use]
    pub fn upgrade_due(&self) -> bool {
        self.stored_mapping
            .is_some_and(|stored| stored < self.current_mapping)
    }

    /// The steps of that upgrade that write rows of the database again, in
    /// the order an open runs them. Each comes with the number of rows it
    /// reads and writes again, where the database records that number.
    ///
    /// Empty for a file that is current. An upgrade can be due and have no
    /// such step: it then only creates tables, triggers and guards.
    #[must_use]
    pub fn steps(&self) -> &[(StorageUpgradeStep, Option<u64>)] {
        &self.steps
    }
}

/// Read which native mapping the database at `path` is stored in and what
/// an open with this Engine would do to bring it to the current one.
///
/// The database file is opened for reading alone and is not changed. A
/// database in write-ahead mode is read as every reader of it is, which can
/// leave its shared-memory file beside it. `key` follows the rules of
/// [`ManagedConnection::open_auto`](crate::ManagedConnection::open_auto).
///
/// The rows of a step are read from the populations the database keeps of
/// its indexes, so the inspection of a large database takes no longer than
/// that of a small one.
///
/// # Errors
///
/// Fails for a key that does not fit the file, for a file this Engine
/// cannot read and for a mapping it does not know, as an open would.
pub fn inspect_storage_upgrade(path: &Path, key: Option<&str>) -> Result<StorageUpgradeInspection> {
    match open_for_reading(path, key)? {
        Some(connection) => Ok(SQLiteRecordStore::inspect_native_mapping(&connection)
            .map_err(VersionError::into_storage_error)?),
        None => Ok(StorageUpgradeInspection::new(
            None,
            SQLiteRecordStore::CURRENT_NATIVE_MAPPING,
            Vec::new(),
        )),
    }
}

/// A connection that reads the file in the form it is stored in, or `None`
/// for a file that is missing or empty.
fn open_for_reading(path: &Path, key: Option<&str>) -> Result<Option<Connection>> {
    let key = match key {
        Some("") => return Err(SQLiteError::EmptyEncryptionKey),
        other => other,
    };
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = match detect_database_file_format(path)? {
        DatabaseFileFormat::Missing => return Ok(None),
        DatabaseFileFormat::PlainSQLite => {
            if key.is_some() {
                return Err(SQLiteError::NotEncrypted);
            }
            Connection::open_with_flags(path, flags)?
        }
        DatabaseFileFormat::Unrecognized => {
            let key = key.ok_or(SQLiteError::EncryptionKeyRequired)?;
            let connection = Connection::open_with_flags(path, flags)?;
            connection.pragma_update(None, "key", key)?;
            connection
        }
        DatabaseFileFormat::CompressedContainer { encrypted } => {
            match (encrypted, key) {
                (true, None) => return Err(SQLiteError::EncryptionKeyRequired),
                (false, Some(_)) => return Err(SQLiteError::NotEncrypted),
                (true, Some(_)) | (false, None) => {}
            }
            // The container's header names its codec; these options decide
            // only the form of a container that does not exist yet.
            compressed_vfs::register_database(path, SQLiteCompressionOptions::default(), key)
                .map_err(SQLiteError::CompressedContainer)?;
            Connection::open_with_flags_and_vfs(path, flags, compressed_vfs::VFS_NAME)?
        }
    };
    // The schema of a mapping before 17 names the Engine's key function in an
    // index, and SQLite reads no table of a schema it cannot parse.
    crate::btree_index::equality::register(&connection)?;
    Ok(Some(connection))
}
