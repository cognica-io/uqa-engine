//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native parallel workers owned through completion of thread-local cleanup.

use std::sync::Mutex;
use std::thread::{self, JoinHandle};

#[derive(Debug)]
pub(super) struct OwnedPool {
    pool: Option<rayon::ThreadPool>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl OwnedPool {
    pub(super) fn start(count: usize) -> Self {
        Self::build(count, spawn_worker).expect("start native parallel workers")
    }

    fn build(
        count: usize,
        mut spawn: impl FnMut(rayon::ThreadBuilder) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<Self, rayon::ThreadPoolBuildError> {
        let mut owner = Self {
            pool: None,
            workers: Mutex::new(Vec::new()),
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(count)
            .thread_name(|index| format!("uqa-execution-{index}"))
            .spawn_handler(|thread| {
                let worker = spawn(thread)?;
                owner
                    .workers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(worker);
                Ok(())
            })
            .build()?;
        owner.pool = Some(pool);
        Ok(owner)
    }

    pub(super) fn install<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        self.pool
            .as_ref()
            .expect("initialized parallel pool")
            .install(work)
    }
}

fn spawn_worker(thread: rayon::ThreadBuilder) -> std::io::Result<JoinHandle<()>> {
    let mut builder = thread::Builder::new();
    if let Some(name) = thread.name() {
        builder = builder.name(name.to_owned());
    }
    if let Some(size) = thread.stack_size() {
        builder = builder.stack_size(size);
    }
    builder.spawn(move || thread.run())
}

impl Drop for OwnedPool {
    fn drop(&mut self) {
        // Rayon signals termination when its pool drops; retain the native
        // handles until every worker and its TLS destructors have finished.
        drop(self.pool.take());
        let workers = self
            .workers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Host work may retain the last executor in a Rayon task. That task
        // must return before this pool can terminate any remaining workers.
        if workers
            .iter()
            .any(|worker| worker.thread().id() == thread::current().id())
        {
            return;
        }
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
