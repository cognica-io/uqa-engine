//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exclusive fresh-file image import keeps the ordinary record owner unavailable until verification and atomic history replacement.

use std::fs::File;

use redb::{Database, ReadableTable};
use uqa_storage::{
    backup::{BackupError, BackupLease, BackupResult, BackupRoot, ImageProvider},
    key_value::{KeyValueImageImportReader, KeyValueImageStep, KeyValueImportUnit},
    mvcc::{BackupCursorKey, DatabaseRestore, DEFAULT_RECEIPT_RETENTION_LIMIT},
    read_control::StorageReadControl,
};

use super::{
    backup_metadata, codec, identifiers, migration, physical_writer, restore, HEADS, METADATA,
    RECORD_FORMAT, TRANSACTIONS, VERSIONS,
};
use crate::error::redb_error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedbBackupImportStep {
    /// One unit or one bounded repository verification window completed. No database session is available yet.
    Pending,
    /// All input and its mapping envelope are verified, the new history is durable, and the import file owner is closed.
    Complete,
}

/// Import a complete redb image into a new, private file. The enclosing restore operation must durably retain `DatabaseRestore` outside that file before construction, keep its directory private through logical catalog/artifact validation, and publish only after this owner has closed. This provider primitive does not create a repository restore intent or install a destination directory.
///
/// Each advance imports at most one record, with its encoded value reserved against the supplied allowance before calling redb's public `insert_reserve`. Values above `maximum_value_bytes` fail explicitly. Repository frames and verification workspace have separate limits; redb's physical page cache is not this decoded-value allowance. An interrupted import retains record format zero, which ordinary opens reject. A failed final commit may have completed; resolve the exact retained request through `RedbStorage::open_restored`, never import into that file again.
pub struct RedbBackupImport<'a> {
    database: Option<Database>,
    input: Option<KeyValueImageImportReader<'a>>,
    root: BackupRoot,
    request: DatabaseRestore,
    control: StorageReadControl,
    maximum_value_bytes: usize,
    complete: bool,
    records: u64,
    identifiers: u64,
}

impl<'a> RedbBackupImport<'a> {
    pub fn new(
        file: File,
        lease: &'a BackupLease<'_>,
        request: DatabaseRestore,
        maximum_value_bytes: usize,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if maximum_value_bytes == 0 {
            return Err(BackupError::Invalid("zero redb import value limit"));
        }
        let root = lease.root();
        if request.source() != root.cut().source {
            return Err(BackupError::Invalid(
                "redb import source differs from restore request",
            ));
        }
        if request.target().as_bytes() == [0; 16] {
            return Err(BackupError::Invalid("zero redb import target identity"));
        }
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(BackupError::Invalid(
                "redb import requires an empty private regular file",
            ));
        }
        let input = KeyValueImageImportReader::new(
            lease,
            ImageProvider::Redb,
            maximum_workspace_bytes,
            control,
        )?;
        let database = Database::builder().create_file(file).map_err(redb_error)?;
        Self::from_image(database, input, request, maximum_value_bytes, control)
    }

    pub(super) fn from_image(
        database: Database,
        input: KeyValueImageImportReader<'a>,
        request: DatabaseRestore,
        maximum_value_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let root = input.root();
        initialize(&database, root, request, control)?;
        Ok(Self {
            database: Some(database),
            input: Some(input),
            root,
            request,
            control: control.clone(),
            maximum_value_bytes,
            complete: false,
            records: 0,
            identifiers: 0,
        })
    }

    pub const fn imported_records(&self) -> u64 {
        self.records
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.identifiers
    }

    pub fn advance(&mut self) -> BackupResult<RedbBackupImportStep> {
        let result = self.advance_inner();
        if result.is_err() {
            self.input = None;
            self.database = None;
        }
        result
    }

    fn advance_inner(&mut self) -> BackupResult<RedbBackupImportStep> {
        if self.complete {
            return Ok(RedbBackupImportStep::Complete);
        }
        let input = self.input.as_mut().ok_or(BackupError::Finished)?;
        let database = self.database.as_ref().expect("active import owns its file");
        match input.advance(16 * 1024)? {
            KeyValueImageStep::Pending => return Ok(RedbBackupImportStep::Pending),
            KeyValueImageStep::Complete => {
                // The reader has retired its verification files and no provider sessions or snapshots were created. The exclusive history owner changes identity and format together.
                activate(database, self.root, self.request, &self.control)?;
                self.input = None;
                self.database = None;
                self.complete = true;
                return Ok(RedbBackupImportStep::Complete);
            }
            KeyValueImageStep::Unit => (),
        }
        match input.current_unit().expect("selected mapping unit") {
            KeyValueImportUnit::Identifier {
                namespace,
                watermark,
            } => {
                let transaction = physical_writer(database)?;
                {
                    let mut table = transaction
                        .open_table(identifiers::TABLE)
                        .map_err(redb_error)?;
                    if table.get(namespace).map_err(redb_error)?.is_some() {
                        return Err(BackupError::Invalid("duplicate imported identifier"));
                    }
                    table
                        .insert(namespace, watermark.to_be_bytes().as_slice())
                        .map_err(redb_error)?;
                    transaction
                        .open_table(backup_metadata::BIRTHS)
                        .map_err(redb_error)?
                        .insert(namespace, (0, 0))
                        .map_err(redb_error)?;
                }
                self.control.cancellation().check()?;
                transaction.commit().map_err(redb_error)?;
                self.identifiers += 1;
            }
            KeyValueImportUnit::Record {
                key,
                revision,
                value_bytes,
            } => {
                if value_bytes > self.maximum_value_bytes as u64 {
                    return Err(BackupError::Limit {
                        resource: "redb import value",
                        required: value_bytes,
                        limit: self.maximum_value_bytes as u64,
                    });
                }
                let length = usize::try_from(value_bytes)
                    .ok()
                    .and_then(|n| n.checked_add(1))
                    .ok_or(BackupError::Invalid("redb import value length overflow"))?;
                let mut owned_key = BackupCursorKey::new(self.control.memory());
                owned_key.extend_from_slice(key)?;
                let _value_reservation = self.control.memory().reserve(length)?;
                let transaction = physical_writer(database)?;
                {
                    let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
                    if heads.get(&*owned_key).map_err(redb_error)?.is_some() {
                        return Err(BackupError::Invalid("duplicate imported record"));
                    }
                    let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
                    let mut value = versions
                        .insert_reserve((&*owned_key, revision.as_u64()), length)
                        .map_err(redb_error)?;
                    let output = value.as_mut();
                    output[0] = 1;
                    let mut offset = 1;
                    while offset < length {
                        let end = length.min(offset.saturating_add(16 * 1024));
                        let read = input.read_current(&mut output[offset..end])?;
                        if read == 0 {
                            return Err(BackupError::Invalid("truncated imported record"));
                        }
                        offset += read;
                    }
                    if input.read_current(&mut [0])? != 0 {
                        return Err(BackupError::Invalid("trailing imported record bytes"));
                    }
                    drop(value);
                    heads
                        .insert(&*owned_key, revision.as_u64())
                        .map_err(redb_error)?;
                }
                self.control.cancellation().check()?;
                transaction.commit().map_err(redb_error)?;
                self.records += 1;
            }
        }
        Ok(RedbBackupImportStep::Pending)
    }
}

fn initialize(
    database: &Database,
    root: BackupRoot,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> BackupResult<()> {
    let transaction = physical_writer(database)?;
    if transaction
        .list_tables()
        .map_err(redb_error)?
        .next()
        .is_some()
        || transaction
            .list_multimap_tables()
            .map_err(redb_error)?
            .next()
            .is_some()
    {
        return Err(BackupError::Invalid(
            "redb import file already contains tables",
        ));
    }
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        metadata
            .insert("database", request.source().as_bytes().as_slice())
            .map_err(redb_error)?;
        metadata
            .insert("backup_target", request.target().as_bytes().as_slice())
            .map_err(redb_error)?;
        metadata
            .insert("backup_image", root.encode().as_slice())
            .map_err(redb_error)?;
        for (name, value) in [
            ("format", 0),
            ("allocated", 0),
            ("sequence", root.cut().sequence.unwrap().as_u64()),
            ("receipt_limit", DEFAULT_RECEIPT_RETENTION_LIMIT),
        ] {
            metadata
                .insert(name, value.to_be_bytes().as_slice())
                .map_err(redb_error)?;
        }
    }
    transaction.open_table(HEADS).map_err(redb_error)?;
    transaction.open_table(VERSIONS).map_err(redb_error)?;
    transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
    transaction
        .open_table(identifiers::TABLE)
        .map_err(redb_error)?;
    backup_metadata::initialize(&transaction, true)?;
    migration::initialize_guard(&transaction)?;
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(())
}

fn activate(
    database: &Database,
    root: BackupRoot,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> BackupResult<()> {
    control.cancellation().check()?;
    let transaction = physical_writer(database)?;
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        if codec::read_u64(&metadata, "format")? != 0
            || codec::database_id(&metadata)? != request.source()
            || metadata
                .get("backup_target")
                .map_err(redb_error)?
                .is_none_or(|v| v.value() != request.target().as_bytes())
            || metadata
                .get("backup_image")
                .map_err(redb_error)?
                .is_none_or(|v| v.value() != root.encode())
        {
            return Err(BackupError::Invalid("redb import activation identity"));
        }
        metadata
            .insert("format", RECORD_FORMAT.to_be_bytes().as_slice())
            .map_err(redb_error)?;
    }
    restore::replace_history(&transaction, request)?;
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(())
}
