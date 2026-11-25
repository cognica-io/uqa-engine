//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select independent keys for coordination files and preserve passphrase-keyed predecessors.

use cipher::zeroize::Zeroizing;
use hmac::{Hmac, Mac};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::Sha256;
use std::{
    path::Path,
    sync::{Arc, Weak},
};
use uqa_storage::{
    encryption_key::{AuxiliaryFile, AuxiliaryFileKeys},
    read_control::StorageReadControl,
    StorageBackendResult, StorageEncryptionKey,
};

use super::{prepare, raw_key, Result, SQLiteError};
use crate::connection::ownership::{DatabaseOwner, RestoreAdmission};

pub(in crate::connection) struct DatabaseKeys {
    root: Zeroizing<[u8; 32]>,
    database: std::path::PathBuf,
    original: StorageEncryptionKey,
    owner: Option<Weak<DatabaseOwner>>,
}

impl DatabaseKeys {
    pub(in crate::connection) fn credential(
        root: Zeroizing<[u8; 32]>,
        database: &Path,
        original: &StorageEncryptionKey,
        owner: Option<&Arc<DatabaseOwner>>,
    ) -> StorageEncryptionKey {
        original.with_file_keys(Arc::new(Self {
            root,
            database: database.to_owned(),
            original: original.clone(),
            owner: owner.map(Arc::downgrade),
        }))
    }

    fn preferred(&self, file: AuxiliaryFile) -> StorageEncryptionKey {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.root.as_slice()).expect("256-bit HMAC key");
        mac.update(b"UQA SQLite auxiliary key v1\0");
        mac.update(match file {
            AuxiliaryFile::Notifications => b"notifications",
            AuxiliaryFile::RelationIdentities => b"relation identities",
            AuxiliaryFile::Serializable => b"serializable transactions",
        });
        let key = Zeroizing::new(<[u8; 32]>::from(mac.finalize().into_bytes()));
        raw_key(key.as_slice(), "")
    }

    fn resolve(&self, file: AuxiliaryFile, path: &Path) -> Result<StorageEncryptionKey> {
        let current = self.preferred(file);
        match path.metadata() {
            Ok(metadata) if metadata.len() == 0 => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
        if authenticates(path, &current)? {
            return Ok(current);
        }
        // The relation coordinator removes its ephemeral registry at the start of the
        // next exclusive epoch. Existing participants must finish with their original key.
        let control = StorageReadControl::with_limit(
            uqa_storage::mvcc::VersionedSessionOptions::default().retained_bytes,
        );
        let owner = self.owner.as_ref().and_then(Weak::upgrade);
        let parent = match (&owner, file) {
            (Some(owner), AuxiliaryFile::Notifications | AuxiliaryFile::Serializable) => {
                owner.sole_owner_admission(&control)?
            }
            _ => None,
        };
        let exclusive = if parent.is_some() {
            match RestoreAdmission::acquire(path, &control) {
                Ok(admission) => Some(admission),
                Err(SQLiteError::DatabaseRestoreBusy) => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        // A concurrent upgrader may have finished between the failed page read and admission.
        if exclusive.is_some() && authenticates(path, &current)? {
            return Ok(current);
        }
        let connection = existing(path)?;
        let legacy = prepare(&connection, &self.original)?;
        application_id(&connection)?;
        if exclusive.is_none() || path.symlink_metadata()?.is_symlink() {
            return Ok(legacy);
        }
        let recognized = match file {
            AuxiliaryFile::Notifications => {
                crate::notifications::recognizes_encrypted_registry(&connection)?
            }
            AuxiliaryFile::Serializable => crate::mvcc::recognizes_encrypted_state(&connection)?,
            AuxiliaryFile::RelationIdentities => false,
        };
        if !recognized {
            return Ok(legacy);
        }
        let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if mode != "delete" {
            return Err(SQLiteError::AuxiliaryJournalMode(mode));
        }
        // SQLCipher rewrites all pages under its own rollback journal. A failed or
        // interrupted conversion retains a complete file under the old or new key.
        connection.pragma_update(None, "rekey", current.expose_secret())?;
        drop(connection);
        if !authenticates(path, &current)? {
            return Err(SQLiteError::StorageBackend(
                "could not verify the converted auxiliary database key".into(),
            ));
        }
        Ok(current)
    }
}

impl AuxiliaryFileKeys for DatabaseKeys {
    fn file_key(
        &self,
        file: AuxiliaryFile,
        path: &Path,
    ) -> StorageBackendResult<StorageEncryptionKey> {
        let expected = file.path(&self.database);
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if path != expected
            && (path.file_name() != expected.file_name()
                || parent.canonicalize().ok().as_deref() != expected.parent())
        {
            return Err(uqa_storage::StorageBackendError::Other(
                "auxiliary file does not belong to this database".into(),
            ));
        }
        self.resolve(file, &expected).map_err(Into::into)
    }
}

fn existing(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

fn application_id(connection: &Connection) -> Result<i64> {
    Ok(connection.pragma_query_value(None, "application_id", |row| row.get(0))?)
}

fn authenticates(path: &Path, key: &StorageEncryptionKey) -> Result<bool> {
    let connection = existing(path)?;
    connection.pragma_update(None, "key", key.expose_secret())?;
    match application_id(&connection) {
        Ok(_) => Ok(true),
        Err(SQLiteError::SQLite(error))
            if error.sqlite_error_code() == Some(ErrorCode::NotADatabase) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
