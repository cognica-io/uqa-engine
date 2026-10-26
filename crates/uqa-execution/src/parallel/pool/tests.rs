//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::parallel::ParallelExecutor;
use std::cell::RefCell;
use std::sync::mpsc::{self, Sender};

struct ThreadExit(Sender<()>);

impl Drop for ThreadExit {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

thread_local! {
    static EXIT: RefCell<Option<ThreadExit>> = const { RefCell::new(None) };
}

#[test]
fn the_last_executor_clone_joins_every_worker_through_tls_cleanup() {
    let executor = ParallelExecutor::new(2);
    let (finished, exited) = mpsc::channel();
    let pool = executor.pool();
    pool.pool.as_ref().unwrap().broadcast(|_| {
        EXIT.with(|exit| *exit.borrow_mut() = Some(ThreadExit(finished.clone())));
    });
    let retained = executor.clone();
    drop(executor);
    assert_eq!(exited.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(retained);
    assert_eq!(exited.try_iter().count(), 2);
}

#[test]
fn configured_workers_bound_concurrency_and_preserve_branch_order() {
    let executor = ParallelExecutor::new(2);
    let barrier = std::sync::Barrier::new(2);
    let threads = Mutex::new(std::collections::HashSet::new());
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let barrier = &barrier;
            let threads = &threads;
            move || {
                threads.lock().unwrap().insert(thread::current().id());
                barrier.wait();
                index
            }
        })
        .collect();
    assert_eq!(
        executor.execute_branches(&workers),
        (0..8).collect::<Vec<_>>()
    );
    assert_eq!(threads.lock().unwrap().len(), 2);
}

#[test]
fn repeated_and_nested_work_reuses_the_owned_pool() {
    let executor = ParallelExecutor::new(2);
    let observe = || (rayon::current_thread_index(), rayon::current_num_threads());
    for _ in 0..3 {
        let results = executor.execute_branches(&[observe; 4]);
        assert!(results
            .iter()
            .all(|(index, count)| index.is_some() && *count == 2));
        let nested = || {
            crate::parallel::run_parallel(vec![1, 2], 8, |chunk| {
                assert_eq!(rayon::current_num_threads(), 2);
                chunk
            })
        };
        assert_eq!(executor.execute_branches(&[nested; 2]), vec![vec![1, 2]; 2]);
    }
}

#[test]
fn failed_pool_construction_joins_workers_already_started() {
    let (finished, exited) = mpsc::channel();
    let mut started = false;
    let result = OwnedPool::build(2, |worker| {
        if started {
            return Err(std::io::Error::other("injected worker creation failure"));
        }
        started = true;
        let finished = finished.clone();
        Ok(thread::spawn(move || {
            EXIT.with(|exit| *exit.borrow_mut() = Some(ThreadExit(finished)));
            worker.run();
        }))
    });
    assert!(result.is_err());
    assert_eq!(exited.try_recv(), Ok(()));
}

#[test]
fn disabled_or_single_branch_execution_creates_no_pool() {
    let disabled = ParallelExecutor::new(0);
    assert_eq!(disabled.execute_branches(&[|| 1; 4]), vec![1; 4]);
    assert!(disabled.pool.get().is_none());
    let single = ParallelExecutor::new(4);
    assert_eq!(single.execute_branches(&[|| 9]), vec![9]);
    assert!(single.pool.get().is_none());
}

#[test]
fn a_host_task_can_release_the_last_executor_on_its_worker() {
    let executor = ParallelExecutor::new(2);
    let retained = executor.clone();
    let (finished, exited) = mpsc::channel();
    executor.pool().pool.as_ref().unwrap().broadcast(|_| {
        EXIT.with(|exit| *exit.borrow_mut() = Some(ThreadExit(finished.clone())));
    });
    let (start, ready) = mpsc::channel();
    executor.pool().install(|| {
        rayon::spawn(move || {
            ready
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            drop(retained);
        });
    });
    drop(executor);
    start.send(()).unwrap();
    for _ in 0..2 {
        exited
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }
}
