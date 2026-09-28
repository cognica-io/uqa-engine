//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    attempt::{Attempt, Connection},
    options,
    subscription::Completion,
    HttpNotificationError as Error, NotificationRetryOptions, NotificationTimeoutStage as Stage,
};
use crate::notifications::{NotificationReady, NotificationWireEvent, ProtocolError};
use std::time::Duration;
use tokio::time::{sleep_until, timeout_at, Instant};
use uqa_core::notifications::{NotificationEpoch, NotificationEvent, NotificationFailureKind};

struct Episode {
    original: Error,
    deadline: Instant,
    attempts: u32,
}

pub(super) async fn run(connection: &Connection, completion: &mut Completion) -> Error {
    let mut previous_epoch = None;
    let mut episode: Option<Episode> = None;
    loop {
        let mut ready = None;
        let error = match run_attempt(
            connection,
            completion,
            previous_epoch,
            &mut episode,
            &mut ready,
        )
        .await
        {
            Ok(()) => Error::protocol(ProtocolError::UnexpectedEnd),
            Err(error) => error,
        };
        let Some(policy) = &connection.options.retry else {
            return error;
        };
        if !error.retryable() {
            return episode.map_or(error.clone(), |episode| {
                Error::exhausted(episode.original, error, episode.attempts)
            });
        }
        if let Some(ready) = ready {
            previous_epoch = Some(ready.identity.epoch);
            let deadline = match options::deadline(policy.episode_timeout) {
                Ok(deadline) => deadline,
                Err(error) => return error,
            };
            if let Err(error) = completion.inbox.push(NotificationEvent::ResyncRequired {
                identity: ready.identity,
                cause: error.kind(),
            }) {
                return error;
            }
            episode = Some(Episode {
                original: error.clone(),
                deadline,
                attempts: 0,
            });
        }
        let Some(episode) = episode.as_mut() else {
            return error;
        };
        if episode.attempts == policy.max_attempts {
            return Error::exhausted(episode.original.clone(), error, episode.attempts);
        }
        let wait = match backoff(policy, episode.attempts, error.retry_after()) {
            Ok(wait) => wait,
            Err(last) => return Error::exhausted(episode.original.clone(), last, episode.attempts),
        };
        let Some(wake) = Instant::now()
            .checked_add(wait)
            .filter(|wake| *wake < episode.deadline)
        else {
            return Error::exhausted(
                episode.original.clone(),
                Error::timeout(Stage::Reconnect),
                episode.attempts,
            );
        };
        sleep_until(wake).await;
        if Instant::now() >= episode.deadline {
            return Error::exhausted(
                episode.original.clone(),
                Error::timeout(Stage::Reconnect),
                episode.attempts,
            );
        }
        episode.attempts += 1;
    }
}

fn backoff(
    policy: &NotificationRetryOptions,
    attempts: u32,
    guidance: Option<Duration>,
) -> Result<Duration, Error> {
    let base = policy.initial_backoff.as_millis() as u64;
    let cap = policy.max_backoff.as_millis() as u64;
    let upper = base
        .saturating_mul(1_u64.checked_shl(attempts).unwrap_or(u64::MAX))
        .min(cap);
    let lower = upper.div_ceil(2);
    let random =
        getrandom::u64().map_err(|_| Error::local(NotificationFailureKind::SourceUnavailable))?;
    let delay = Duration::from_millis(lower + random % (upper - lower + 1));
    Ok(delay.max(guidance.unwrap_or(Duration::ZERO)))
}

async fn run_attempt(
    connection: &Connection,
    completion: &mut Completion,
    previous_epoch: Option<NotificationEpoch>,
    episode: &mut Option<Episode>,
    ready: &mut Option<NotificationReady>,
) -> Result<(), Error> {
    let mut attempt = connection
        .open(episode.as_ref().map(|episode| episode.deadline))
        .await?;
    loop {
        if Instant::now() >= attempt.deadline() {
            return Err(attempt.timeout());
        }
        if attempt.ended() {
            let event = attempt
                .finish()?
                .ok_or_else(|| Error::protocol(ProtocolError::UnexpectedEnd))?;
            forward(
                event,
                &mut attempt,
                completion,
                previous_epoch,
                episode,
                ready,
            )?;
            continue;
        }
        let deadline = attempt.deadline();
        let timeout_error = attempt.timeout();
        let chunk = timeout_at(deadline, attempt.response.chunk())
            .await
            .map_err(|_| timeout_error)?
            .map_err(Error::transport)?;
        let Some(chunk) = chunk else {
            if let Some(event) = attempt.finish()? {
                forward(
                    event,
                    &mut attempt,
                    completion,
                    previous_epoch,
                    episode,
                    ready,
                )?;
            }
            continue;
        };
        if chunk.len() > connection.options.max_transport_chunk_bytes {
            return Err(Error::local(NotificationFailureKind::Capacity));
        }
        if !chunk.is_empty() {
            attempt.received_bytes()?;
        }
        let mut offset = 0;
        while offset < chunk.len() {
            if Instant::now() >= attempt.deadline() {
                return Err(attempt.timeout());
            }
            let step = attempt
                .decoder
                .decode(&chunk[offset..])
                .map_err(Error::protocol)?;
            offset += step.consumed;
            if let Some(event) = step.event {
                let result = forward(
                    event,
                    &mut attempt,
                    completion,
                    previous_epoch,
                    episode,
                    ready,
                );
                if result.is_err() && attempt.decoder.is_terminal() && offset < chunk.len() {
                    attempt
                        .decoder
                        .decode(&chunk[offset..])
                        .map_err(Error::protocol)?;
                }
                result?;
                tokio::task::yield_now().await;
            }
        }
    }
}

fn forward(
    event: NotificationWireEvent,
    attempt: &mut Attempt,
    completion: &mut Completion,
    previous_epoch: Option<NotificationEpoch>,
    episode: &mut Option<Episode>,
    ready: &mut Option<NotificationReady>,
) -> Result<(), Error> {
    match event {
        NotificationWireEvent::Ready(value) => {
            if previous_epoch == Some(value.identity.epoch) {
                return Err(Error::protocol(ProtocolError::Identity));
            }
            attempt.admit_ready(&value)?;
            completion.ready(&value)?;
            *ready = Some(value);
            *episode = None;
            Ok(())
        }
        NotificationWireEvent::Notification(event) => completion.inbox.push(event),
        NotificationWireEvent::Heartbeat => Ok(()),
        NotificationWireEvent::Error(error) => Err(Error::remote(error)),
        NotificationWireEvent::ServerDraining { .. } => Err(Error::draining()),
    }
}
