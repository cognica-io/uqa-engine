//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Repository operations for the standalone client and its current SQL session.

use crate::migration_io::DatabaseBackend;
use crate::Session;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uqa_engine::backup::{
    BackupId, BackupKey, BackupLimits, BackupOptions, LocalRepository, Protection, ProtectionId,
    RestoreDestination, RestoreId, RestoreOptions, SQLiteBackupMapping, SQLiteBackupTarget,
    StorageEncryptionKey, StorageReadControl,
};

use uqa_engine::SQLiteCompressionOptions;

mod arguments;
pub(super) use arguments::USAGE;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug)]
pub(super) enum Command {
    Help,
    Init(RepositorySettings),
    Repository(RepositorySettings, Operation),
}

#[derive(Debug)]
pub(super) struct RepositorySettings {
    directory: PathBuf,
    credentials: Credentials,
    memory_bytes: usize,
    workspace_bytes: u64,
}

#[derive(Debug)]
pub(super) enum Credentials {
    Plaintext,
    Protected { domain: ProtectionId, file: PathBuf },
}

impl Credentials {
    fn protection(&self) -> Result<Protection> {
        let Self::Protected { domain, file } = self else {
            return Ok(Protection::Plaintext);
        };
        let mut input = File::open(file)
            .map_err(|error| format!("open repository key file {}: {error}", file.display()))?;
        let mut key = [0u8; 32];
        input
            .read_exact(&mut key)
            .map_err(|error| format!("read 32-byte repository key: {error}"))?;
        if input.read(&mut [0u8; 1])? != 0 {
            return Err("a repository key file must contain exactly 32 bytes".into());
        }
        Ok(Protection::Encrypted {
            domain: *domain,
            key: BackupKey::new(key),
        })
    }
}

#[derive(Debug)]
pub(super) enum Operation {
    Create(BackupOptions),
    List,
    Verify(BackupId),
    Remove(BackupId),
    Collect(usize),
    Copy {
        id: BackupId,
        path: PathBuf,
        destination: Credentials,
        allow_plaintext: bool,
        maximum_encoded_bytes: u64,
    },
    Publish(BackupId),
    Restore(Restore),
}

#[derive(Debug)]
pub(super) struct Restore {
    request: RestoreRequest,
    target: Target,
    options: RestoreOptions,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum RestoreRequest {
    Begin(BackupId),
    Resume(RestoreId),
    Cancel(RestoreId),
}

#[derive(Debug)]
pub(super) enum Target {
    Memory,
    Directory {
        directory: PathBuf,
        storage: Option<DatabaseBackend>,
        key_file: Option<PathBuf>,
        compression: Option<SQLiteCompressionOptions>,
    },
}

fn hex_id(bytes: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(bytes))
}

fn open(
    path: &Path,
    credentials: &Credentials,
    control: &StorageReadControl,
) -> Result<LocalRepository> {
    Ok(LocalRepository::open(
        path,
        credentials.protection()?,
        BackupLimits::default(),
        control,
    )?)
}

impl Command {
    pub(super) fn uses_database(&self) -> bool {
        matches!(
            self,
            Self::Repository(_, Operation::Create(_) | Operation::Restore(_))
        )
    }

    pub(super) fn execute(
        &self,
        session: &mut Session,
        out: &mut impl Write,
        shell: bool,
    ) -> Result<()> {
        let (settings, operation) = match self {
            Self::Help => return Ok(writeln!(out, "{USAGE}")?),
            Self::Init(settings) => (settings, None),
            Self::Repository(settings, operation) => (settings, Some(operation)),
        };
        let control = StorageReadControl::with_limit(settings.memory_bytes)
            .with_temporary_limit(settings.workspace_bytes);
        let _interrupt = crate::interrupt::QueryInterrupt::new(control.cancellation().clone())?;
        let Some(operation) = operation else {
            let repository = LocalRepository::create(
                &settings.directory,
                settings.credentials.protection()?,
                BackupLimits::default(),
                &control,
            )?;
            return Ok(writeln!(out, "{}", hex_id(repository.id().as_bytes()))?);
        };
        let repository = open(&settings.directory, &settings.credentials, &control)?;
        operation.execute(
            session,
            &repository,
            &control,
            settings.workspace_bytes,
            shell,
            out,
        )
    }
}

impl Operation {
    fn execute(
        &self,
        session: &mut Session,
        repository: &LocalRepository,
        control: &StorageReadControl,
        workspace_bytes: u64,
        shell: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        match self {
            Self::Create(options) => {
                let mut job = session.engine.begin_backup(repository, *options, control)?;
                writeln!(
                    std::io::stderr().lock(),
                    "backup ID {}",
                    hex_id(job.id().as_bytes())
                )?;
                writeln!(out, "{}", hex_id(job.finish()?.backup().as_bytes()))?;
            }
            Self::List => {
                let mut list = repository.list_backups(workspace_bytes, control)?;
                while let Some(root) = list.next_backup()? {
                    let parent = root
                        .parent()
                        .map_or_else(|| "-".to_string(), |id| hex_id(id.as_bytes()));
                    writeln!(out, "{}\t{parent}", hex_id(root.backup().as_bytes()))?;
                }
            }
            Self::Verify(id) => {
                repository
                    .open_backup(*id, control)?
                    .verify(workspace_bytes, control)?;
                writeln!(out, "{}", hex_id(id.as_bytes()))?;
            }
            Self::Remove(id) => repository.remove_backup(*id, control)?,
            Self::Collect(files) => {
                let report = repository.collect_garbage(*files, workspace_bytes, control)?;
                writeln!(
                    out,
                    "removed_objects={} removed_staging_files={} remaining_files={}",
                    report.removed_objects, report.removed_staging_files, report.remaining_files
                )?;
            }
            Self::Copy {
                id,
                path,
                destination,
                allow_plaintext,
                maximum_encoded_bytes,
            } => {
                let to = open(path, destination, control)?;
                let root = repository.copy_backup_to(
                    *id,
                    &to,
                    *maximum_encoded_bytes,
                    workspace_bytes,
                    *allow_plaintext,
                    control,
                )?;
                writeln!(out, "{}", hex_id(root.backup().as_bytes()))?;
            }
            Self::Publish(id) => {
                let mut operation = repository.resume_publication(*id, workspace_bytes, control)?;
                let root = operation.publish(control)?;
                writeln!(out, "{}", hex_id(root.backup().as_bytes()))?;
            }
            Self::Restore(restore) => restore.execute(session, repository, control, shell, out)?,
        }
        Ok(())
    }
}

impl Restore {
    fn execute(
        &self,
        session: &mut Session,
        repository: &LocalRepository,
        control: &StorageReadControl,
        shell: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        let (destination, backend, path, key) = match &self.target {
            Target::Memory => {
                if !shell {
                    return Err("use \\backup in a running session to restore into :memory:".into());
                }
                (
                    RestoreDestination::Memory,
                    DatabaseBackend::default(),
                    None,
                    None,
                )
            }
            Target::Directory {
                directory,
                storage,
                key_file,
                compression,
            } => {
                let backend = storage.unwrap_or(session.backend);
                let key = key_file
                    .as_deref()
                    .map(crate::database_key_file)
                    .transpose()?;
                let destination =
                    backend.restore_destination(directory, key.as_deref(), *compression)?;
                (destination, backend, Some(directory.join("database")), key)
            }
        };
        let options = self.options;
        let restored =
            match self.request {
                RestoreRequest::Cancel(id) => {
                    session
                        .engine
                        .cancel_restore(repository, id, destination, options, control)?
                }
                RestoreRequest::Begin(id) => Some(finish_restore(session.engine.begin_restore(
                    repository,
                    id,
                    destination,
                    options,
                    control,
                )?)?),
                RestoreRequest::Resume(id) => Some(finish_restore(
                    session
                        .engine
                        .resume_restore(repository, id, destination, options, control)?,
                )?),
            };
        if let Some(restored) = restored {
            let location = path
                .as_deref()
                .map_or_else(|| ":memory:".to_string(), |p| p.display().to_string());
            if shell {
                session.install_engine(restored.into_retained(), path, key, backend)?;
            }
            writeln!(out, "{}", crate::printing::terminal_text(&location))?;
        }
        Ok(())
    }
}

fn finish_restore(
    mut job: uqa_engine::backup::RestoreJob<'_>,
) -> Result<uqa_engine::backup::RestoredEngine> {
    if let Some(id) = job.id() {
        writeln!(
            std::io::stderr().lock(),
            "restore request ID {}",
            hex_id(id.as_bytes())
        )?;
    }
    Ok(job.finish()?)
}

impl DatabaseBackend {
    fn restore_destination(
        self,
        directory: &Path,
        key: Option<&str>,
        compression: Option<SQLiteCompressionOptions>,
    ) -> Result<RestoreDestination> {
        let mapping = match self {
            Self::Redb => {
                if compression.is_some() {
                    return Err("target compression requires SQLite storage".into());
                }
                if key.is_some() {
                    return Err("redb does not support a database encryption key".into());
                }
                return Ok(RestoreDestination::persistent(
                    directory,
                    uqa_storage_redb::RedbBackupTarget::default(),
                ));
            }
            Self::SQLiteNative => SQLiteBackupMapping::Native,
            Self::SQLiteKeyValue => SQLiteBackupMapping::KeyValue,
        };
        let mut target = SQLiteBackupTarget::new(mapping);
        target.encryption_key = key.map(StorageEncryptionKey::new);
        target.compression = compression;
        Ok(RestoreDestination::persistent(directory, target))
    }
}
