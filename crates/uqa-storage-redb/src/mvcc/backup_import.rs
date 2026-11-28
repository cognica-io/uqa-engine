//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exclusive fresh-file image import keeps the ordinary record owner unavailable until verification and atomic history replacement.

use std::{fs::File, path::Path};

use redb::{Database, ReadableDatabase, ReadableTable, WriteTransaction};
use uqa_storage::{
    backup::{
        BackupError, BackupImageImportStep, BackupLeaseInput, BackupRecordImageWriter,
        BackupResult, BackupRoot, ImageProvider,
    },
    key_value::{KeyValueImageImportDriver, KeyValueImageImportReader, KeyValueImageWriter},
    mvcc::{CommitSequence, DatabaseRestore, DEFAULT_RECEIPT_RETENTION_LIMIT},
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
pub struct RedbBackupImport<'a>(KeyValueImageImportDriver<'a, ImageWriter>);

impl<'a> RedbBackupImport<'a> {
    /// Resolve the atomic completion of this exact archive and history request. Partial imports keep format zero and reject; completed imports retain later writes. No input records or history are replayed.
    pub fn recover(
        path: &Path,
        root: BackupRoot,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        control.check()?;
        if root.cut().format.provider() != ImageProvider::Redb
            || root.cut().source != request.source()
        {
            return Err(BackupError::Invalid(
                "redb import recovery mapping or source",
            ));
        }
        let database = Database::open(path).map_err(redb_error)?;
        let transaction = database.begin_read().map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        if codec::read_u64(&metadata, "format")? != RECORD_FORMAT
            || codec::database_id(&metadata)? != request.target()
            || metadata
                .get("backup_target")
                .map_err(redb_error)?
                .is_none_or(|value| value.value() != request.target().as_bytes())
            || metadata
                .get("backup_image")
                .map_err(redb_error)?
                .is_none_or(|value| value.value() != root.encode())
        {
            return Err(BackupError::Invalid(
                "redb import recovery identity or incomplete input",
            ));
        }
        control.check()?;
        Ok(())
    }

    pub fn new(
        file: File,
        lease: impl Into<BackupLeaseInput<'a>>,
        request: DatabaseRestore,
        maximum_value_bytes: usize,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if maximum_value_bytes == 0 {
            return Err(BackupError::Invalid("zero redb import value limit"));
        }
        let lease = lease.into();
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
        Ok(Self(KeyValueImageImportDriver::new(
            input,
            ImageWriter {
                transaction: None,
                database,
            },
            request,
            maximum_value_bytes,
            "redb import value",
            control,
        )))
    }

    pub const fn imported_records(&self) -> u64 {
        self.0.imported_records()
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.0.imported_identifiers()
    }

    pub fn advance(&mut self) -> BackupResult<RedbBackupImportStep> {
        self.advance_with_window(16 * 1024)
    }

    /// Advance with a caller-selected whole-object verification window. At most one record is imported, under its existing admitted value limit.
    pub fn advance_with_window(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<RedbBackupImportStep> {
        Ok(match self.0.advance(maximum_object_bytes)? {
            BackupImageImportStep::Pending => RedbBackupImportStep::Pending,
            BackupImageImportStep::Imported => RedbBackupImportStep::Complete,
        })
    }
}

struct ImageWriter {
    // An unfinished private transaction aborts before its database file closes.
    transaction: Option<WriteTransaction>,
    database: Database,
}

impl ImageWriter {
    fn transaction(&mut self) -> BackupResult<&WriteTransaction> {
        if self.transaction.is_none() {
            self.transaction = Some(physical_writer(&self.database)?);
        }
        Ok(self
            .transaction
            .as_ref()
            .expect("active private write transaction"))
    }
}

impl BackupRecordImageWriter for ImageWriter {
    fn identifier(
        &mut self,
        namespace: &[u8],
        watermark: u64,
        _control: &StorageReadControl,
    ) -> BackupResult<()> {
        let transaction = self.transaction()?;
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
        Ok(())
    }

    fn record(
        &mut self,
        key: &[u8],
        revision: CommitSequence,
        length: usize,
        read: &mut dyn FnMut(&mut [u8]) -> BackupResult<usize>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let size = super::heads::stored_size(revision.as_u64(), length)?;
        let _value_reservation = control.memory().reserve(size)?;
        let transaction = self.transaction()?;
        let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
        if heads.get(key).map_err(redb_error)?.is_some() {
            return Err(BackupError::Invalid("duplicate imported record"));
        }
        let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        super::heads::insert_from(
            &mut super::heads::VersionWriter::new(transaction, Some(control)),
            &mut heads,
            &mut versions,
            key,
            revision.as_u64(),
            length,
            |destination| read_record_image(read, revision, destination),
        )
    }

    fn flush(&mut self, control: &StorageReadControl) -> BackupResult<()> {
        if let Some(transaction) = self.transaction.take() {
            control.cancellation().check()?;
            transaction.commit().map_err(redb_error)?;
        }
        Ok(())
    }
}

impl KeyValueImageWriter for ImageWriter {
    fn activate(
        self,
        root: BackupRoot,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        activate(&self.database, root, request, control)
    }
}

fn read_record_image(
    read_payload: &mut dyn FnMut(&mut [u8]) -> BackupResult<usize>,
    revision: uqa_storage::mvcc::CommitSequence,
    destination: &mut [u8],
) -> BackupResult<uqa_storage::backup::ImageDigest> {
    let mut hash = uqa_storage::backup::RecordImageHasher::new(revision);
    let mut remaining = destination;
    while !remaining.is_empty() {
        let length = remaining.len().min(16 * 1024);
        let read = read_payload(&mut remaining[..length])?;
        if read == 0 {
            return Err(BackupError::Invalid("truncated imported record"));
        }
        hash.update(&remaining[..read]);
        remaining = &mut remaining[read..];
    }
    if read_payload(&mut [0])? != 0 {
        return Err(BackupError::Invalid("trailing imported record bytes"));
    }
    Ok(hash.finish())
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
            (super::image_fingerprints::LEGACY_THROUGH, 0),
            ("sequence", root.cut().sequence.unwrap().as_u64()),
            ("receipt_limit", DEFAULT_RECEIPT_RETENTION_LIMIT),
        ] {
            metadata
                .insert(name, value.to_be_bytes().as_slice())
                .map_err(redb_error)?;
        }
    }
    transaction
        .open_table(super::image_fingerprints::TABLE)
        .map_err(redb_error)?;
    drop(super::large_values::Writer::open(&transaction)?);
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
