//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated wire observations and closed diagnostic metadata.

use super::{NotificationTiming, ProtocolError};
use std::fmt;
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind, NotificationIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationReady {
    pub identity: NotificationIdentity,
    pub accepted_channel_count: usize,
    pub timing: NotificationTiming,
}

/// A syntactically bounded remote failure. Unknown codes remain available explicitly but never appear in diagnostic formatting or acquire automatic retry authority.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerFailure {
    pub identity: NotificationIdentity,
    code: Box<str>,
    pub retryable: bool,
}

impl ServerFailure {
    pub(super) fn new(
        identity: NotificationIdentity,
        code: String,
        retryable: bool,
    ) -> Result<Self, ProtocolError> {
        if code.is_empty()
            || code.len() > 64
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ProtocolError::InvalidFields);
        }
        Ok(Self {
            identity,
            code: code.into_boxed_str(),
            retryable,
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn known_kind(&self) -> Option<NotificationFailureKind> {
        use NotificationFailureKind as Kind;
        [
            Kind::InvalidRequest,
            Kind::Authentication,
            Kind::AuthorityRevoked,
            Kind::Unsupported,
            Kind::Capacity,
            Kind::Backpressure,
            Kind::Protocol,
            Kind::SourceUnavailable,
            Kind::Transport,
            Kind::Timeout,
            Kind::ServerDraining,
            Kind::Cancelled,
            Kind::SequenceExhausted,
        ]
        .into_iter()
        .find(|kind| kind.code() == self.code.as_ref())
    }
}

impl fmt::Debug for ServerFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerFailure")
            .field("identity", &self.identity)
            .field("kind", &self.known_kind())
            .field("retryable", &self.retryable)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationWireEvent {
    Ready(NotificationReady),
    /// The decoder only produces the Core `Notification` variant; gap and reconnection events belong to transport lifecycle.
    Notification(NotificationEvent),
    Error(ServerFailure),
    ServerDraining {
        identity: NotificationIdentity,
    },
    /// An empty/comment-only SSE block; it consumes no notification sequence.
    Heartbeat,
}
