//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Command, Credentials, Operation, RepositorySettings, Restore, RestoreRequest, Target};
use std::path::PathBuf;
use uqa_engine::backup::{
    BackupId, BackupOptions, ProtectionId, RestoreId, RestoreOptions, DEFAULT_BACKUP_MEMORY_BYTES,
    DEFAULT_BACKUP_WORKSPACE_BYTES,
};
use uqa_engine::SQLiteCompressionOptions;

pub(crate) const USAGE: &str = "\
usql [--db FILE] [--storage sqlite-native|sqlite-kv|redb] backup COMMAND REPOSITORY [OPTIONS]
\\backup COMMAND REPOSITORY [OPTIONS]

  init                              Create a repository in a new directory
  create [--parent ID]              Capture a full or incremental backup
  list                              List each backup ID and its parent
  verify ID                         Verify a completed backup
  remove ID                         Remove a completed backup
  gc [--max-files N]                Collect up to N unused files (default 256)
  copy ID DEST_REPO                 Copy a standalone backup into an existing repository
  resume-publication ID             Publish a prepared backup after interruption
  restore ID DEST                   Restore into a new directory or, in the shell, :memory:
  resume-restore REQUEST_ID DEST    Resume an interrupted persistent restore
  cancel-restore REQUEST_ID DEST    Cancel an interrupted persistent restore

Repository protection: --domain HEX --repository-key-file FILE (32 raw bytes).
Copy destination protection: --destination-domain HEX --destination-key-file FILE.
Capture/copy: --allow-plaintext explicitly permits export to a plaintext repository.
All commands accept --memory-bytes N and --workspace-bytes N.
Capture and copy accept --max-encoded-bytes N.
Restore accepts --target-storage sqlite-native|sqlite-kv|redb, --target-key-file FILE,
  --target-compression lz4|zstd (SQLite), --max-value-bytes N and --retained-bytes N.";

struct Arguments<'a>(Vec<&'a str>);

impl<'a> Arguments<'a> {
    fn flag_index(&self, name: &str) -> Result<Option<usize>, String> {
        let mut positions = self.0.iter().enumerate().filter(|(_, v)| **v == name);
        let first = positions.next().map(|(index, _)| index);
        if positions.next().is_some() {
            return Err(format!("duplicate {name}"));
        }
        Ok(first)
    }

    fn value(&mut self, name: &str) -> Result<Option<&'a str>, String> {
        let Some(index) = self.flag_index(name)? else {
            return Ok(None);
        };
        let value = self
            .0
            .get(index + 1)
            .copied()
            .filter(|s| !s.starts_with("--") && !s.is_empty())
            .ok_or_else(|| format!("{name} requires a value"))?;
        self.0.drain(index..=index + 1);
        Ok(Some(value))
    }

    fn number<T: std::str::FromStr>(&mut self, name: &str, default: T) -> Result<T, String> {
        match self.value(name)? {
            Some(value) => value
                .parse()
                .map_err(|_| format!("invalid {name}: {value}")),
            None => Ok(default),
        }
    }

    fn flag(&mut self, name: &str) -> Result<bool, String> {
        if let Some(index) = self.flag_index(name)? {
            self.0.remove(index);
            return Ok(true);
        }
        Ok(false)
    }

    fn positional(&mut self, description: &str) -> Result<&'a str, String> {
        if self
            .0
            .first()
            .is_none_or(|arg| arg.is_empty() || arg.starts_with("--"))
        {
            return Err(format!("expected {description}"));
        }
        Ok(self.0.remove(0))
    }

    fn credentials(&mut self, domain_flag: &str, key_flag: &str) -> Result<Credentials, String> {
        match (self.value(domain_flag)?, self.value(key_flag)?) {
            (None, None) => Ok(Credentials::Plaintext),
            (Some(domain), Some(file)) => Ok(Credentials::Protected {
                domain: ProtectionId::from_bytes(identity(domain)?)
                    .map_err(|error| error.to_string())?,
                file: PathBuf::from(file),
            }),
            _ => Err(format!(
                "{domain_flag} and {key_flag} must be supplied together"
            )),
        }
    }

    fn restore(&mut self, verb: &str, workspace_bytes: u64) -> Result<Operation, String> {
        let storage = self
            .value("--target-storage")?
            .map(str::parse)
            .transpose()?;
        let key_file = self.value("--target-key-file")?.map(PathBuf::from);
        let compression = match self.value("--target-compression")? {
            Some("lz4") => Some(SQLiteCompressionOptions::lz4()),
            Some("zstd") => Some(SQLiteCompressionOptions::zstd()),
            Some(value) => {
                return Err(format!(
                    "unknown target compression {value:?}; expected lz4 or zstd"
                ))
            }
            None => None,
        };
        let defaults = RestoreOptions::default();
        let options = RestoreOptions {
            maximum_workspace_bytes: workspace_bytes,
            maximum_value_bytes: self.number("--max-value-bytes", defaults.maximum_value_bytes)?,
            retained_bytes: self.number("--retained-bytes", defaults.retained_bytes)?,
        };
        let id = self.positional("a backup or restore request ID")?;
        let request = if verb == "restore" {
            RestoreRequest::Begin(backup_id(id)?)
        } else {
            let id = RestoreId::from_bytes(identity(id)?).map_err(|error| error.to_string())?;
            if verb == "resume-restore" {
                RestoreRequest::Resume(id)
            } else {
                RestoreRequest::Cancel(id)
            }
        };
        let target = match self.positional("a new destination directory or :memory:")? {
            ":memory:" if storage.is_some() || key_file.is_some() || compression.is_some() => {
                return Err("a memory destination cannot take persistent storage options".into())
            }
            ":memory:" if !matches!(request, RestoreRequest::Begin(_)) => {
                return Err("a restore request ID requires a persistent destination".into())
            }
            ":memory:" => Target::Memory,
            path => Target::Directory {
                directory: path.into(),
                storage,
                key_file,
                compression,
            },
        };
        Ok(Operation::Restore(Restore {
            request,
            target,
            options,
        }))
    }
}

fn identity(input: &str) -> Result<[u8; 16], String> {
    if input.len() != 32 || !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected a 32-digit hexadecimal identity".into());
    }
    Ok(u128::from_str_radix(input, 16)
        .map_err(|error| error.to_string())?
        .to_be_bytes())
}

fn backup_id(input: &str) -> Result<BackupId, String> {
    BackupId::from_bytes(identity(input)?).map_err(|error| error.to_string())
}

impl Command {
    pub(crate) fn parse(input: &[String]) -> Result<Self, String> {
        let mut args = Arguments(input.iter().map(String::as_str).collect());
        if matches!(args.0.as_slice(), [] | ["--help" | "-h" | "help"]) {
            return Ok(Self::Help);
        }
        let verb = args.positional("a backup command")?;
        let credentials = args.credentials("--domain", "--repository-key-file")?;
        let memory_bytes = args.number("--memory-bytes", DEFAULT_BACKUP_MEMORY_BYTES)?;
        let workspace_bytes = args.number("--workspace-bytes", DEFAULT_BACKUP_WORKSPACE_BYTES)?;
        let settings = RepositorySettings {
            directory: PathBuf::from(args.positional("a repository directory")?),
            credentials,
            memory_bytes,
            workspace_bytes,
        };
        let operation = match verb {
            "init" if args.0.is_empty() => return Ok(Self::Init(settings)),
            "init" => return Err("unexpected arguments to backup init".into()),
            "create" => Operation::Create(BackupOptions {
                parent: args.value("--parent")?.map(backup_id).transpose()?,
                allow_plaintext_export: args.flag("--allow-plaintext")?,
                maximum_encoded_bytes: args.number(
                    "--max-encoded-bytes",
                    BackupOptions::default().maximum_encoded_bytes,
                )?,
                maximum_workspace_bytes: workspace_bytes,
            }),
            "list" => Operation::List,
            "verify" => Operation::Verify(backup_id(args.positional("a backup ID")?)?),
            "remove" => Operation::Remove(backup_id(args.positional("a backup ID")?)?),
            "gc" => Operation::Collect(args.number("--max-files", 256)?),
            "copy" => {
                let destination =
                    args.credentials("--destination-domain", "--destination-key-file")?;
                let allow_plaintext = args.flag("--allow-plaintext")?;
                let maximum_encoded_bytes = args.number(
                    "--max-encoded-bytes",
                    BackupOptions::default().maximum_encoded_bytes,
                )?;
                let id = backup_id(args.positional("a backup ID")?)?;
                let path = args.positional("a destination repository")?.into();
                Operation::Copy {
                    id,
                    path,
                    destination,
                    allow_plaintext,
                    maximum_encoded_bytes,
                }
            }
            "restore" | "resume-restore" | "cancel-restore" => {
                args.restore(verb, workspace_bytes)?
            }
            "resume-publication" => Operation::Publish(backup_id(args.positional("a backup ID")?)?),
            _ => return Err(format!("unknown backup command {verb:?}")),
        };
        if let Some(argument) = args.0.first() {
            return Err(format!("unexpected argument {argument:?} to backup {verb}"));
        }
        Ok(Self::Repository(settings, operation))
    }
}
