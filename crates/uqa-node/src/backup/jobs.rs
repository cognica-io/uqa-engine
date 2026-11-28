//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::options::{
    backup_id, positive, restore_id, BackupOptions, RestoreDestination, RestoreOptions,
};
use super::{
    core, release, with_job, work, Arc, AsyncTask, BackupRepository, BackupRoot, BackupTask,
    BigInt, Buffer, Mutex, Result,
};
use crate::engine::Engine;
use crate::input::runtime_error;
use napi_derive::napi;

#[napi(object)]
pub struct BackupCapabilities {
    pub export: bool,
    pub import: bool,
    pub snapshot_fingerprints: bool,
}

#[napi(object)]
pub struct BackupProgress {
    pub phase: String,
    pub visited_positions: BigInt,
    pub copied_payload_bytes: BigInt,
    pub metadata_read_bytes: BigInt,
    pub excluded_units: BigInt,
    pub parent_units: BigInt,
    pub hashed_payload_bytes: BigInt,
    pub certified_reused_units: BigInt,
    pub certified_reused_bytes: BigInt,
    pub reused_units: BigInt,
    pub reused_payload_bytes: BigInt,
    pub written_units: BigInt,
    pub written_payload_bytes: BigInt,
    pub written_object_bytes: BigInt,
    pub verified_object_bytes: BigInt,
}

impl From<core::BackupCaptureProgress> for BackupProgress {
    fn from(value: core::BackupCaptureProgress) -> Self {
        Self {
            phase: value.phase.as_str().into(),
            visited_positions: value.source.visited_positions.into(),
            copied_payload_bytes: value.source.copied_payload_bytes.into(),
            metadata_read_bytes: value.source.metadata_read_bytes.into(),
            excluded_units: value.source.excluded_units.into(),
            parent_units: value.image.parent_units.into(),
            hashed_payload_bytes: value.image.hashed_payload_bytes.into(),
            certified_reused_units: value.image.certified_reused_units.into(),
            certified_reused_bytes: value.image.certified_reused_bytes.into(),
            reused_units: value.image.reused_units.into(),
            reused_payload_bytes: value.image.reused_payload_bytes.into(),
            written_units: value.image.written_units.into(),
            written_payload_bytes: value.image.written_payload_bytes.into(),
            written_object_bytes: value.image.written_object_bytes.into(),
            verified_object_bytes: value.verified_object_bytes.into(),
        }
    }
}

#[napi(object)]
pub struct BackupReport {
    pub units: BigInt,
    pub decoded_bytes: BigInt,
    pub image_digest: Buffer,
    pub progress: BackupProgress,
}

#[napi]
pub struct BackupJob {
    id: core::BackupId,
    inner: Arc<Mutex<Option<core::OwnedBackupJob>>>,
}

#[napi]
pub struct RestoreJob {
    id: Option<core::RestoreId>,
    backup: core::BackupId,
    inner: Arc<Mutex<Option<core::OwnedRestoreJob>>>,
}

enum RestoreRequest {
    Begin(core::BackupId),
    Resume(core::RestoreId),
}

impl RestoreJob {
    fn new(job: core::OwnedRestoreJob) -> Self {
        Self {
            id: job.id(),
            backup: job.backup(),
            inner: Arc::new(Mutex::new(Some(job))),
        }
    }
}

fn restore_job(
    authority: &Engine,
    repository: &BackupRepository,
    request: RestoreRequest,
    destination: RestoreDestination,
    options: Option<RestoreOptions>,
) -> Result<AsyncTask<BackupTask<RestoreJob>>> {
    let engine = authority.inner()?.clone();
    let (options, control) = options
        .unwrap_or_default()
        .convert(repository.memory_bytes)?;
    let repository = repository.owner()?;
    let destination = destination.convert()?;
    Ok(work(move || {
        let configuration = options.with_runtime(&engine);
        match request {
            RestoreRequest::Begin(id) => core::OwnedRestoreJob::begin(
                &engine,
                repository,
                id,
                destination,
                configuration,
                &control,
            ),
            RestoreRequest::Resume(id) => core::OwnedRestoreJob::resume(
                &engine,
                repository,
                id,
                destination,
                configuration,
                &control,
            ),
        }
        .map(RestoreJob::new)
        .map_err(runtime_error)
    }))
}

#[napi]
impl Engine {
    #[napi]
    pub fn backup_capabilities(&self) -> Result<BackupCapabilities> {
        let c = self.inner()?.backup_capabilities();
        Ok(BackupCapabilities {
            export: c.export,
            import: c.import,
            snapshot_fingerprints: c.snapshot_fingerprints,
        })
    }

    #[napi(ts_return_type = "Promise<BackupJob>")]
    pub fn begin_backup(
        &self,
        repository: &BackupRepository,
        options: Option<BackupOptions>,
    ) -> Result<AsyncTask<BackupTask<BackupJob>>> {
        let engine = self.inner()?.clone();
        let (options, control) = options
            .unwrap_or_default()
            .convert(repository.memory_bytes)?;
        let repository = repository.owner()?;
        Ok(work(move || {
            let job = core::OwnedBackupJob::begin(&engine, repository, options, &control)
                .map_err(runtime_error)?;
            Ok(BackupJob {
                id: job.id(),
                inner: Arc::new(Mutex::new(Some(job))),
            })
        }))
    }

    #[napi(ts_return_type = "Promise<RestoreJob>")]
    pub fn begin_restore(
        &self,
        repository: &BackupRepository,
        backup: Buffer,
        destination: RestoreDestination,
        options: Option<RestoreOptions>,
    ) -> Result<AsyncTask<BackupTask<RestoreJob>>> {
        restore_job(
            self,
            repository,
            RestoreRequest::Begin(backup_id(&backup)?),
            destination,
            options,
        )
    }

    #[napi(ts_return_type = "Promise<RestoreJob>")]
    pub fn resume_restore(
        &self,
        repository: &BackupRepository,
        id: Buffer,
        destination: RestoreDestination,
        options: Option<RestoreOptions>,
    ) -> Result<AsyncTask<BackupTask<RestoreJob>>> {
        restore_job(
            self,
            repository,
            RestoreRequest::Resume(restore_id(&id)?),
            destination,
            options,
        )
    }
}

#[napi]
impl BackupJob {
    /// Release the capture and its repository leases. Use cancel to record cancellation.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn close(&self) -> AsyncTask<BackupTask<()>> {
        release(&self.inner)
    }

    #[napi(getter)]
    pub fn id(&self) -> Buffer {
        self.id.as_bytes().to_vec().into()
    }

    #[napi(ts_return_type = "Promise<BackupProgress>")]
    pub fn progress(&self) -> AsyncTask<BackupTask<BackupProgress>> {
        with_job(&self.inner, |job| Ok(job.progress().into()))
    }

    #[napi(ts_return_type = "Promise<BackupReport | null>")]
    pub fn report(&self) -> AsyncTask<BackupTask<Option<BackupReport>>> {
        with_job(&self.inner, |job| {
            Ok(job.report().map(|r| BackupReport {
                units: r.inventory.units.into(),
                decoded_bytes: r.inventory.decoded_bytes.into(),
                image_digest: r.inventory.image_digest.as_bytes().to_vec().into(),
                progress: job.progress().into(),
            }))
        })
    }

    #[napi(ts_return_type = "Promise<'pending' | 'prepared' | 'complete'>")]
    pub fn advance(
        &self,
        maximum_positions: f64,
        maximum_bytes: f64,
    ) -> Result<AsyncTask<BackupTask<String>>> {
        let positions = positive(maximum_positions, "maximumPositions")?;
        let bytes = positive(maximum_bytes, "maximumBytes")?;
        Ok(with_job(&self.inner, move |job| {
            Ok(job
                .advance(positions, bytes)
                .map_err(runtime_error)?
                .as_str()
                .to_owned())
        }))
    }

    #[napi(ts_return_type = "Promise<BackupRoot>")]
    pub fn publish(&self) -> AsyncTask<BackupTask<BackupRoot>> {
        with_job(&self.inner, |job| {
            job.publish().map(Into::into).map_err(runtime_error)
        })
    }

    #[napi(ts_return_type = "Promise<BackupRoot>")]
    pub fn finish(&self) -> AsyncTask<BackupTask<BackupRoot>> {
        with_job(&self.inner, |job| {
            job.finish().map(Into::into).map_err(runtime_error)
        })
    }

    #[napi(ts_return_type = "Promise<BackupRoot | null>")]
    pub fn cancel(&self) -> AsyncTask<BackupTask<Option<BackupRoot>>> {
        with_job(&self.inner, |job| {
            job.cancel()
                .map(|r| r.map(Into::into))
                .map_err(runtime_error)
        })
    }
}

#[napi]
impl RestoreJob {
    /// Release this handle and its leases. A durable restore can be resumed by ID.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn close(&self) -> AsyncTask<BackupTask<()>> {
        release(&self.inner)
    }

    #[napi(getter)]
    pub fn id(&self) -> Option<Buffer> {
        self.id.map(|id| id.as_bytes().to_vec().into())
    }

    #[napi(getter)]
    pub fn backup(&self) -> Buffer {
        self.backup.as_bytes().to_vec().into()
    }

    #[napi(ts_return_type = "Promise<RestorePhase>")]
    pub fn phase(&self) -> AsyncTask<BackupTask<String>> {
        with_job(&self.inner, |job| Ok(job.phase().as_str().to_owned()))
    }

    #[napi(ts_return_type = "Promise<RestorePhase>")]
    pub fn advance(&self, maximum_object_bytes: f64) -> Result<AsyncTask<BackupTask<String>>> {
        let bytes = positive(maximum_object_bytes, "maximumObjectBytes")?;
        Ok(with_job(&self.inner, move |job| {
            job.advance(bytes)
                .map(|value| value.as_str().to_owned())
                .map_err(runtime_error)
        }))
    }

    #[napi(ts_return_type = "Promise<Engine | null>")]
    pub fn advance_publication(
        &self,
        maximum_steps: f64,
    ) -> Result<AsyncTask<BackupTask<Option<Engine>>>> {
        let steps = positive(maximum_steps, "maximumSteps")?;
        Ok(with_job(&self.inner, move |job| {
            job.advance_publication(steps)
                .map(|r| r.map(Engine::from_restored))
                .map_err(runtime_error)
        }))
    }

    #[napi(ts_return_type = "Promise<Engine>")]
    pub fn publish(&self) -> AsyncTask<BackupTask<Engine>> {
        with_job(&self.inner, |job| {
            job.publish()
                .map(Engine::from_restored)
                .map_err(runtime_error)
        })
    }

    #[napi(ts_return_type = "Promise<Engine>")]
    pub fn finish(&self) -> AsyncTask<BackupTask<Engine>> {
        with_job(&self.inner, |job| {
            job.finish()
                .map(Engine::from_restored)
                .map_err(runtime_error)
        })
    }

    #[napi(ts_return_type = "Promise<Engine | null>")]
    pub fn cancel(&self) -> AsyncTask<BackupTask<Option<Engine>>> {
        with_job(&self.inner, |job| {
            job.cancel()
                .map(|r| r.map(Engine::from_restored))
                .map_err(runtime_error)
        })
    }
}
