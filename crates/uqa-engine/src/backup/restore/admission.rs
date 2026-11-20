//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorize on the calling session and construct private provider input within that transaction.

use super::{
    persistent, start_error, Admission, BackupError, BackupId, Budgeted, DiskANNTemporaryBudget,
    Engine, ImageProvider, Input, LocalRepository, MemoryBudget, MemoryImageImportReader,
    RestoreConfiguration, RestoreDestination, RestoreId, RestoreJob, SQLError, State,
    StorageReadControl,
};

pub(in crate::backup) enum RestoreRequest {
    Begin(BackupId),
    Resume(RestoreId),
}

pub(in crate::backup) struct AuthorizedRestore {
    request: RestoreRequest,
    destination: RestoreDestination,
    configuration: RestoreConfiguration,
    control: StorageReadControl,
}

impl RestoreRequest {
    pub(in crate::backup) fn admit<T>(
        self,
        engine: &Engine,
        destination: RestoreDestination,
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
        start: impl FnOnce(AuthorizedRestore) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        engine.with_direct_query_snapshot(
            true,
            |engine| {
                if matches!(self, Self::Begin(_)) {
                    control.check().map_err(|error| start_error(error.into()))?;
                }
                let configuration = options.into();
                engine.authorize_restore(configuration.options)?;
                start(AuthorizedRestore {
                    request: self,
                    destination,
                    configuration,
                    control: control.clone(),
                })
            },
            std::convert::identity,
        )
    }
}

impl AuthorizedRestore {
    pub(in crate::backup) fn open(
        self,
        repository: &LocalRepository,
    ) -> Result<RestoreJob<'_>, SQLError> {
        match self.request {
            RestoreRequest::Begin(backup) => self.begin(repository, backup),
            RestoreRequest::Resume(id) => self.resume(repository, id),
        }
    }

    fn begin(
        self,
        repository: &LocalRepository,
        backup: BackupId,
    ) -> Result<RestoreJob<'_>, SQLError> {
        let options = self.configuration.options;
        let admitted = self.configuration.admit();
        let control = self
            .control
            .with_temporary_limit(options.maximum_workspace_bytes);
        let lease = repository
            .open_backup(backup, &control)
            .map_err(start_error)?;
        let root = lease.root();
        match self.destination {
            RestoreDestination::Memory => {
                if root.cut().format.provider() != ImageProvider::Memory {
                    return Err(start_error(BackupError::Unsupported(
                        "restore destination mapping",
                    )));
                }
                let memory = control
                    .memory()
                    .reserve(size_of::<Input<'_>>())
                    .map_err(|e| start_error(e.into()))?;
                let functions = Admission::open(&lease, &control).map_err(start_error)?;
                let input = Input::new(
                    MemoryImageImportReader::new(lease, options.maximum_workspace_bytes, &control)
                        .map_err(start_error)?,
                    admitted.bindings.analyzers.clone().unwrap_or_default(),
                    DiskANNTemporaryBudget::new(options.maximum_workspace_bytes),
                    options.maximum_value_bytes,
                    &control,
                )
                .map_err(start_error)?;
                Ok(RestoreJob {
                    state: State::Reading(
                        Budgeted::new(Box::new(input), memory),
                        admitted.bindings,
                        functions,
                    ),
                    backup,
                    root: Some(root),
                    id: None,
                    retained: MemoryBudget::new(options.retained_bytes),
                    control,
                })
            }
            RestoreDestination::Persistent { directory, target } => {
                if root.cut().format.provider() != target.image_provider() {
                    return Err(start_error(BackupError::Unsupported(
                        "restore destination mapping",
                    )));
                }
                drop(lease);
                let memory = control
                    .memory()
                    .reserve(size_of::<persistent::Persistent<'_>>())
                    .map_err(|e| start_error(e.into()))?;
                let candidate = persistent::Persistent::begin(
                    repository, backup, directory, target, admitted, &control,
                )
                .map_err(start_error)?;
                let id = candidate.id();
                Ok(RestoreJob {
                    state: State::Persistent(Budgeted::new(Box::new(candidate), memory)),
                    backup,
                    root: Some(root),
                    id: Some(id),
                    retained: MemoryBudget::new(options.retained_bytes),
                    control,
                })
            }
        }
    }

    fn resume(
        self,
        repository: &LocalRepository,
        id: RestoreId,
    ) -> Result<RestoreJob<'_>, SQLError> {
        let RestoreDestination::Persistent { directory, target } = self.destination else {
            return Err(start_error(BackupError::Unsupported(
                "durable memory restore recovery",
            )));
        };
        let options = self.configuration.options;
        let control = self
            .control
            .with_temporary_limit(options.maximum_workspace_bytes);
        let memory = control
            .memory()
            .reserve(size_of::<persistent::Persistent<'_>>())
            .map_err(|e| start_error(e.into()))?;
        let (candidate, backup) = persistent::Persistent::resume(
            repository,
            id,
            directory,
            target,
            self.configuration.admit(),
            &control,
        )
        .map_err(start_error)?;
        Ok(RestoreJob {
            state: State::Persistent(Budgeted::new(Box::new(candidate), memory)),
            backup,
            root: None,
            id: Some(id),
            retained: MemoryBudget::new(options.retained_bytes),
            control,
        })
    }
}
