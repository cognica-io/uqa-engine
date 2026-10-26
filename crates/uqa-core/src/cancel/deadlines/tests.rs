//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cancel::{CancellationToken, QueryCancelled};
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};

struct ThreadExit(Sender<()>);

impl Drop for ThreadExit {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

thread_local! {
    static EXIT: RefCell<Option<ThreadExit>> = const { RefCell::new(None) };
}

/// Observe actual thread-local destruction, beyond returning from the queue loop.
fn observe_exit(service: &Arc<DeadlineService>) -> Receiver<()> {
    let (exited, receiver) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let action = service.schedule(Duration::ZERO, move || {
        EXIT.with(|exit| *exit.borrow_mut() = Some(ThreadExit(exited)));
        ready.send(()).unwrap();
    });
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(action);
    receiver
}

#[test]
fn final_owner_joins_the_timer_through_thread_local_cleanup() {
    let service = DeadlineService::start();
    let exited = observe_exit(&service);
    let retained = service.schedule(Duration::from_secs(3_600), || panic!("disarmed action"));
    drop(service);
    assert_eq!(exited.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(retained);
    assert_eq!(exited.try_recv(), Ok(()));
}

#[test]
fn one_token_reuses_its_timer_until_its_last_deadline_ends() {
    let service = DeadlineService::start();
    let exited = observe_exit(&service);
    let token = CancellationToken::new();
    token.state.deadlines.set(Arc::clone(&service)).unwrap();
    drop(service);
    for _ in 0..10 {
        let deadline = token.deadline(
            Duration::from_secs(3_600),
            CancellationReason::StatementTimeout,
        );
        drop(deadline);
        assert_eq!(exited.try_recv(), Err(mpsc::TryRecvError::Empty));
    }
    let deadline = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::StatementTimeout,
    );
    drop(token);
    assert_eq!(exited.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(deadline);
    assert_eq!(exited.try_recv(), Ok(()));
}

#[test]
fn dropping_an_action_can_disarm_its_captured_action_without_locking_the_queue_twice() {
    let (finished, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let service = DeadlineService::start();
        let exited = observe_exit(&service);
        let inner = service.schedule(Duration::from_secs(3_600), || {
            panic!("disarmed inner action")
        });
        let outer = service.schedule(Duration::from_secs(3_600), move || drop(inner));
        drop(service);
        drop(outer);
        assert_eq!(exited.try_recv(), Ok(()));
        finished.send(()).unwrap();
    });
    receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.join().unwrap();
}

#[test]
fn an_action_can_release_the_last_timer_owner_on_its_own_thread() {
    let service = DeadlineService::start();
    let exited = observe_exit(&service);
    let handle = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&handle);
    let (start, ready) = mpsc::channel();
    let action = service.schedule(Duration::ZERO, move || {
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        drop(captured.lock().unwrap().take());
    });
    *handle.lock().unwrap() = Some(action);
    drop(service);
    start.send(()).unwrap();
    exited.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(handle.lock().unwrap().is_none());
}

#[test]
fn concurrent_tokens_release_one_shared_worker_after_all_deadlines_end() {
    let service = DeadlineService::start();
    let exited = observe_exit(&service);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let token = CancellationToken::new();
            token.state.deadlines.set(Arc::clone(&service)).unwrap();
            scope.spawn(move || {
                for _ in 0..100 {
                    let deadline = token.deadline(
                        Duration::from_secs(3_600),
                        CancellationReason::StatementTimeout,
                    );
                    drop(deadline);
                    assert_eq!(token.check(), Ok(()));
                }
            });
        }
    });
    assert_eq!(exited.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(service);
    assert_eq!(exited.try_recv(), Ok(()));
}

fn dequeue(deadline: &CancellationDeadline) -> Pending {
    deadline
        .service
        .state
        .lock()
        .pending
        .remove(&deadline.key)
        .expect("armed deadline")
}

#[test]
fn dropped_dequeued_deadline_cannot_cancel_the_next_statement() {
    let token = CancellationToken::new();
    let deadline = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::StatementTimeout,
    );
    let pending = dequeue(&deadline);
    drop(deadline);

    let next = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::StatementTimeout,
    );
    pending.fire();
    assert_eq!(token.check(), Ok(()));

    dequeue(&next).fire();
    assert_eq!(
        token.check(),
        Err(QueryCancelled::new(CancellationReason::StatementTimeout))
    );
    drop(next);
    assert_eq!(token.check(), Ok(()));
}

#[test]
fn fired_deadline_drop_preserves_an_unrelated_cancellation() {
    let token = CancellationToken::new();
    let deadline = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::StatementTimeout,
    );
    token.cancel();
    dequeue(&deadline).fire();
    drop(deadline);
    assert_eq!(token.check(), Err(QueryCancelled::USER_REQUEST));
}

#[test]
fn disarmed_dequeued_session_deadline_cannot_terminate_the_session() {
    let token = CancellationToken::new();
    let deadline = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::IdleSessionTimeout,
    );
    let pending = dequeue(&deadline);
    drop(deadline);
    pending.fire();
    assert_eq!(token.termination(), None);
    assert_eq!(token.check(), Ok(()));
}

#[test]
fn fired_session_deadline_remains_terminal_after_drop_and_reset() {
    let token = CancellationToken::new();
    let deadline = token.deadline(
        Duration::from_secs(3_600),
        CancellationReason::IdleSessionTimeout,
    );
    dequeue(&deadline).fire();
    drop(deadline);
    token.reset();
    assert_eq!(
        token.termination(),
        Some(CancellationReason::IdleSessionTimeout)
    );
    assert_eq!(
        token.check(),
        Err(QueryCancelled::new(CancellationReason::IdleSessionTimeout))
    );
}
