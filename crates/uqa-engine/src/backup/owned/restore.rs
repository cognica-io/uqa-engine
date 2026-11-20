//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::backup::restore::{AuthorizedRestore, RestoreRequest};
use crate::backup::{
    start_error, BackupError, BackupId, BackupResult, LocalRepository, RestoreConfiguration,
    RestoreDestination, RestoreId, RestoreJob, RestorePhase, RestoredEngine, StorageReadControl,
};
use crate::{Engine, SQLError};
use std::{
    io,
    sync::{mpsc, Arc},
    thread,
};

type Operation = Box<dyn FnOnce(&mut RestoreJob<'_>) + Send>;

/// A restore handle usable across host threads. Private provider transactions and validation continuations stay on the thread that admitted them. Dropping the handle releases the repository lease and provider resources before returning.
pub struct OwnedRestoreJob {
    requests: mpsc::SyncSender<Option<Operation>>,
    worker: Option<thread::JoinHandle<()>>,
    backup: BackupId,
    id: Option<RestoreId>,
}

fn disconnected() -> BackupError {
    io::Error::new(io::ErrorKind::BrokenPipe, "restore worker disconnected").into()
}

impl OwnedRestoreJob {
    pub fn begin(
        engine: &Engine,
        repository: Arc<LocalRepository>,
        backup: BackupId,
        destination: RestoreDestination,
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        RestoreRequest::Begin(backup).admit(engine, destination, options, control, |request| {
            Self::start(repository, request)
        })
    }

    pub fn resume(
        engine: &Engine,
        repository: Arc<LocalRepository>,
        id: RestoreId,
        destination: RestoreDestination,
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        RestoreRequest::Resume(id).admit(engine, destination, options, control, |request| {
            Self::start(repository, request)
        })
    }

    fn start(
        repository: Arc<LocalRepository>,
        request: AuthorizedRestore,
    ) -> Result<Self, SQLError> {
        let (requests, operations) = mpsc::sync_channel::<Option<Operation>>(0);
        let (ready, admission) = mpsc::sync_channel(0);
        let worker = thread::Builder::new()
            .name("uqa-restore".into())
            .spawn(move || match request.open(&repository) {
                Ok(mut job) => {
                    if ready.send(Ok((job.backup(), job.id()))).is_ok() {
                        while let Ok(Some(operation)) = operations.recv() {
                            operation(&mut job);
                        }
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|error| start_error(error.into()))?;
        let (backup, id) = admission
            .recv()
            .map_err(|_| start_error(disconnected()))??;
        Ok(Self {
            requests,
            worker: Some(worker),
            backup,
            id,
        })
    }

    fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut RestoreJob<'_>) -> BackupResult<T> + Send + 'static,
    ) -> BackupResult<T> {
        let (reply, result) = mpsc::sync_channel(0);
        self.requests
            .send(Some(Box::new(move |job| {
                let _ = reply.send(operation(job));
            })))
            .map_err(|_| disconnected())?;
        result.recv().map_err(|_| disconnected())?
    }

    pub fn id(&self) -> Option<RestoreId> {
        self.id
    }

    pub fn backup(&self) -> BackupId {
        self.backup
    }

    pub fn phase(&self) -> RestorePhase {
        self.call(|job| Ok(job.phase()))
            .unwrap_or(RestorePhase::Failed)
    }

    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<RestorePhase> {
        self.call(move |job| job.advance(maximum_object_bytes))
    }

    pub fn advance_publication(
        &mut self,
        maximum_steps: usize,
    ) -> BackupResult<Option<RestoredEngine>> {
        self.call(move |job| job.advance_publication(maximum_steps))
    }

    pub fn publish(&mut self) -> BackupResult<RestoredEngine> {
        self.call(|job| job.publish())
    }

    pub fn finish(&mut self) -> BackupResult<RestoredEngine> {
        self.call(|job| job.finish())
    }

    pub fn cancel(&mut self) -> BackupResult<Option<RestoredEngine>> {
        self.call(|job| job.cancel())
    }
}

impl Drop for OwnedRestoreJob {
    fn drop(&mut self) {
        let _ = self.requests.send(None);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
