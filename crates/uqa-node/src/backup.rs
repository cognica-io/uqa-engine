//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native repository and job adapters. All filesystem and job operations run on libuv workers.

mod jobs;
mod options;
mod retention;

use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer, ToNapiValue, TypeName};
use napi::{Env, Error, Result, Task};
use napi_derive::napi;
use uqa_engine::backup as core;

use super::input::runtime_error;
use options::{backup_id, optional_workspace, BackupRepositoryOptions};

pub struct BackupTask<T>(Option<Box<dyn FnOnce() -> Result<T> + Send>>);

impl<T: ToNapiValue + TypeName + Send + 'static> Task for BackupTask<T> {
    type Output = T;
    type JsValue = T;

    fn compute(&mut self) -> Result<T> {
        (self
            .0
            .take()
            .ok_or_else(|| Error::from_reason("backup task already ran"))?)()
    }

    fn resolve(&mut self, _env: Env, output: T) -> Result<T> {
        Ok(output)
    }
}

fn work<T: ToNapiValue + TypeName + Send + 'static>(
    action: impl FnOnce() -> Result<T> + Send + 'static,
) -> AsyncTask<BackupTask<T>> {
    AsyncTask::new(BackupTask(Some(Box::new(action))))
}

fn locked<J: Send + 'static, T: ToNapiValue + TypeName + Send + 'static>(
    job: &Arc<Mutex<J>>,
    action: impl FnOnce(&mut J) -> Result<T> + Send + 'static,
) -> AsyncTask<BackupTask<T>> {
    let job = Arc::clone(job);
    work(move || {
        let mut owner = job
            .lock()
            .map_err(|_| Error::from_reason("backup handle is poisoned"))?;
        action(&mut owner)
    })
}

fn with_job<J: Send + 'static, T: ToNapiValue + TypeName + Send + 'static>(
    handle: &Arc<Mutex<Option<J>>>,
    action: impl FnOnce(&mut J) -> Result<T> + Send + 'static,
) -> AsyncTask<BackupTask<T>> {
    locked(handle, |slot| {
        action(
            slot.as_mut()
                .ok_or_else(|| Error::from_reason("backup job is closed"))?,
        )
    })
}

fn release<J: Send + 'static>(handle: &Arc<Mutex<Option<J>>>) -> AsyncTask<BackupTask<()>> {
    locked(handle, |slot| {
        *slot = None;
        Ok(())
    })
}

#[napi(object)]
pub struct BackupRoot {
    pub id: Buffer,
    pub parent: Option<Buffer>,
    pub repository: Buffer,
    pub units: BigInt,
    pub decoded_bytes: BigInt,
    pub image_digest: Buffer,
}

impl From<core::BackupRoot> for BackupRoot {
    fn from(root: core::BackupRoot) -> Self {
        let summary = root.summary();
        Self {
            id: root.backup().as_bytes().to_vec().into(),
            parent: root.parent().map(|p| p.as_bytes().to_vec().into()),
            repository: root.repository().as_bytes().to_vec().into(),
            units: summary.units.into(),
            decoded_bytes: summary.decoded_bytes.into(),
            image_digest: summary.image_digest.as_bytes().to_vec().into(),
        }
    }
}

#[napi]
pub struct BackupRepository {
    inner: Option<Arc<core::LocalRepository>>,
    id: [u8; 16],
    memory_bytes: usize,
}

#[napi(ts_return_type = "Promise<BackupRepository>")]
pub fn create_backup_repository(
    path: String,
    options: Option<BackupRepositoryOptions>,
) -> Result<AsyncTask<BackupTask<BackupRepository>>> {
    repository(path, options, true)
}

#[napi(ts_return_type = "Promise<BackupRepository>")]
pub fn open_backup_repository(
    path: String,
    options: Option<BackupRepositoryOptions>,
) -> Result<AsyncTask<BackupTask<BackupRepository>>> {
    repository(path, options, false)
}

fn repository(
    path: String,
    options: Option<BackupRepositoryOptions>,
    create: bool,
) -> Result<AsyncTask<BackupTask<BackupRepository>>> {
    let (protection, limits, memory_bytes) = options.unwrap_or_default().convert()?;
    Ok(work(move || {
        let control = core::StorageReadControl::with_limit(memory_bytes);
        let repo = if create {
            core::LocalRepository::create(path, protection, limits, &control)
        } else {
            core::LocalRepository::open(path, protection, limits, &control)
        }
        .map_err(runtime_error)?;
        Ok(BackupRepository {
            id: repo.id().as_bytes(),
            inner: Some(Arc::new(repo)),
            memory_bytes,
        })
    }))
}

impl BackupRepository {
    fn owner(&self) -> Result<Arc<core::LocalRepository>> {
        self.inner
            .clone()
            .ok_or_else(|| Error::from_reason("backup repository is closed"))
    }
}

#[napi]
impl BackupRepository {
    /// Release this handle. Active jobs and listings retain their own repository resources.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn close(&mut self) -> AsyncTask<BackupTask<()>> {
        let owned = self.inner.take();
        work(move || {
            drop(owned);
            Ok(())
        })
    }

    #[napi(getter)]
    pub fn id(&self) -> Buffer {
        self.id.to_vec().into()
    }

    #[napi(ts_return_type = "Promise<BackupRoot>")]
    pub fn verify(
        &self,
        id: Buffer,
        maximum_workspace_bytes: Option<f64>,
    ) -> Result<AsyncTask<BackupTask<BackupRoot>>> {
        let id = backup_id(&id)?;
        let workspace = optional_workspace(maximum_workspace_bytes)?;
        let repo = self.owner()?;
        let control = core::StorageReadControl::with_limit(self.memory_bytes);
        Ok(work(move || {
            let lease = repo.open_backup(id, &control).map_err(runtime_error)?;
            lease.verify(workspace, &control).map_err(runtime_error)?;
            Ok(lease.root().into())
        }))
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn remove(&self, id: Buffer) -> Result<AsyncTask<BackupTask<()>>> {
        let id = backup_id(&id)?;
        let repo = self.owner()?;
        let control = core::StorageReadControl::with_limit(self.memory_bytes);
        Ok(work(move || {
            repo.remove_backup(id, &control).map_err(runtime_error)
        }))
    }

    #[napi(ts_return_type = "Promise<BackupListing>")]
    pub fn list(
        &self,
        maximum_workspace_bytes: Option<f64>,
    ) -> Result<AsyncTask<BackupTask<BackupListing>>> {
        let repo = self.owner()?;
        let workspace = optional_workspace(maximum_workspace_bytes)?;
        let control = core::StorageReadControl::with_limit(self.memory_bytes);
        Ok(work(move || {
            let listing = repo
                .list_backups(workspace, &control)
                .map_err(runtime_error)?;
            Ok(BackupListing {
                inner: Arc::new(Mutex::new(Some(listing))),
            })
        }))
    }
}

#[napi]
pub struct BackupListing {
    inner: Arc<Mutex<Option<core::BackupListing>>>,
}

#[napi]
impl BackupListing {
    #[napi(ts_return_type = "Promise<BackupRoot | null>")]
    pub fn next(&self) -> AsyncTask<BackupTask<Option<BackupRoot>>> {
        locked(&self.inner, |slot| {
            let Some(listing) = slot.as_mut() else {
                return Ok(None);
            };
            let root = listing.next_backup().map_err(runtime_error)?;
            if root.is_none() {
                *slot = None;
            }
            Ok(root.map(Into::into))
        })
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn close(&self) -> AsyncTask<BackupTask<()>> {
        release(&self.inner)
    }
}
