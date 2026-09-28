//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    attempt::Connection, inbox::Inbox, reconnect, HttpNotificationError as Error,
    HttpNotificationOptions, NotificationCancellation,
};
use crate::notifications::{NotificationReady, SubscriptionRequest};
use reqwest::Url;
use secrecy::SecretString;
use std::{fmt, sync::Arc};
use tokio::{sync::oneshot, task::JoinHandle};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind, NotificationIdentity};

/// A bounded independently read HTTP subscription. Closing joins its one worker; dropping signals cancellation and aborts that same task. Received events never execute SQL.
pub struct HttpNotificationSubscription {
    inbox: Arc<Inbox>,
    worker: WorkerOwner,
    initial_ready: NotificationReady,
    visible_identity: NotificationIdentity,
    closed: bool,
}

impl HttpNotificationSubscription {
    pub fn initial_ready(&self) -> &NotificationReady {
        &self.initial_ready
    }
    /// Identity visible to this consumer; advances when Reconnected is consumed, never ahead of its gap event.
    pub fn identity(&self) -> &NotificationIdentity {
        &self.visible_identity
    }
    pub fn cancellation(&self) -> NotificationCancellation {
        self.worker.cancellation.clone()
    }

    /// Cancelling this receive future leaves the subscription alive. Close/drop or its explicit cancellation signal stops the owned transport.
    pub async fn next_event(&mut self) -> Result<Option<NotificationEvent>, Error> {
        if self.closed {
            return Ok(None);
        }
        loop {
            let notified = self.inbox.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.worker.cancellation.is_cancelled() {
                return Err(Error::cancelled());
            }
            if let Some(result) = self.inbox.take() {
                if let Ok(Some(NotificationEvent::Reconnected { identity })) = &result {
                    self.visible_identity = identity.clone();
                }
                return result;
            }
            tokio::select! {
                biased;
                () = self.worker.cancellation.cancelled() => return Err(Error::cancelled()),
                () = &mut notified => {}
            }
        }
    }

    /// Completes local worker/response cleanup. It does not acknowledge a remote listener-cleanup transaction.
    pub async fn close(&mut self) -> Result<(), Error> {
        self.worker.cancellation.cancel();
        self.inbox.finish(Ok(()));
        self.worker.join().await?;
        self.closed = true;
        Ok(())
    }
}

impl fmt::Debug for HttpNotificationSubscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpNotificationSubscription")
            .field("identity", &self.visible_identity)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

struct WorkerOwner {
    cancellation: NotificationCancellation,
    task: Option<JoinHandle<()>>,
}

impl WorkerOwner {
    async fn join(&mut self) -> Result<(), Error> {
        if let Some(task) = self.task.as_mut() {
            let result = task.await;
            self.task = None;
            result.map_err(|_| Error::local(NotificationFailureKind::SourceUnavailable))?;
        }
        Ok(())
    }
}

impl Drop for WorkerOwner {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(super) struct Completion {
    initial: Option<oneshot::Sender<Result<NotificationReady, Error>>>,
    pub inbox: Arc<Inbox>,
    cancellation: NotificationCancellation,
    finished: bool,
}

impl Completion {
    pub fn ready(&mut self, ready: &NotificationReady) -> Result<(), Error> {
        if self.cancellation.is_cancelled() {
            return Err(Error::cancelled());
        }
        if let Some(initial) = self.initial.take() {
            initial
                .send(Ok(ready.clone()))
                .map_err(|_| Error::cancelled())
        } else {
            self.inbox.push(NotificationEvent::Reconnected {
                identity: ready.identity.clone(),
            })
        }
    }

    fn finish(&mut self, error: Error) {
        self.inbox.finish(Err(error.clone()));
        if let Some(initial) = self.initial.take() {
            let _ = initial.send(Err(error));
        }
        self.finished = true;
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(if self.cancellation.is_cancelled() {
                Error::cancelled()
            } else {
                Error::local(NotificationFailureKind::SourceUnavailable)
            });
        }
    }
}

pub(crate) async fn subscribe(
    endpoint: Url,
    credential: SecretString,
    channels: &[&str],
    options: HttpNotificationOptions,
    cancellation: NotificationCancellation,
) -> Result<HttpNotificationSubscription, Error> {
    if cancellation.is_cancelled() {
        return Err(Error::cancelled());
    }
    options.validate()?;
    tokio::runtime::Handle::try_current().map_err(|_| Error::invalid_options())?;
    let request = Arc::new(
        SubscriptionRequest::new(channels, options.channel_limit()).map_err(Error::request)?,
    );
    let inbox = Arc::new(Inbox::new(&options)?);
    let connection = Connection::new(endpoint, credential, request, options)?;
    let (sender, receiver) = oneshot::channel();
    let mut completion = Completion {
        initial: Some(sender),
        inbox: Arc::clone(&inbox),
        cancellation: cancellation.clone(),
        finished: false,
    };
    let worker_cancel = cancellation.clone();
    let task = tokio::spawn(async move {
        let error = tokio::select! {
            biased;
            () = worker_cancel.cancelled() => Error::cancelled(),
            error = reconnect::run(&connection, &mut completion) => error,
        };
        completion.finish(error);
    });
    let mut worker = WorkerOwner {
        cancellation,
        task: Some(task),
    };
    let ready = receiver
        .await
        .unwrap_or_else(|_| Err(Error::local(NotificationFailureKind::SourceUnavailable)));
    match ready {
        Ok(initial_ready) => Ok(HttpNotificationSubscription {
            inbox,
            worker,
            visible_identity: initial_ready.identity.clone(),
            initial_ready,
            closed: false,
        }),
        Err(error) => {
            worker.cancellation.cancel();
            worker.join().await?;
            Err(error)
        }
    }
}
