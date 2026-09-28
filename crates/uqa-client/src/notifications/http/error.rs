//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::notifications::{ProtocolError, ServerFailure};
use std::error::Error as _;
use std::{fmt, sync::Arc, time::Duration};
use uqa_core::notifications::{NotificationFailureKind as Kind, NotificationRequestId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationTimeoutStage {
    Connection,
    Readiness,
    Idle,
    Reconnect,
}

/// A retained failure with content-free Display/Debug. Private transport and server diagnostics are available only through explicit accessors.
#[derive(Clone)]
pub struct HttpNotificationError(Arc<Failure>);

struct Failure {
    kind: Kind,
    retryable: bool,
    cause: Cause,
}

enum Cause {
    Local,
    Protocol(ProtocolError),
    Transport(reqwest::Error),
    Timeout(NotificationTimeoutStage),
    Remote(ServerFailure),
    Response {
        status: u16,
        code: Box<str>,
        message: Box<str>,
        request_id: NotificationRequestId,
        retry_after: Option<Duration>,
    },
    Exhausted {
        original: HttpNotificationError,
        last: HttpNotificationError,
        attempts: u32,
    },
}

impl HttpNotificationError {
    pub fn kind(&self) -> Kind {
        self.0.kind
    }
    pub fn code(&self) -> &'static str {
        self.kind().code()
    }
    pub fn timeout_stage(&self) -> Option<NotificationTimeoutStage> {
        match &self.0.cause {
            Cause::Timeout(stage) => Some(*stage),
            Cause::Transport(error) if error.is_timeout() && error.is_connect() => {
                Some(NotificationTimeoutStage::Connection)
            }
            _ => None,
        }
    }
    pub fn protocol_error(&self) -> Option<ProtocolError> {
        match &self.0.cause {
            Cause::Protocol(error) => Some(*error),
            _ => None,
        }
    }
    pub fn transport_error(&self) -> Option<&reqwest::Error> {
        match &self.0.cause {
            Cause::Transport(error) => Some(error),
            _ => None,
        }
    }
    pub fn server_failure(&self) -> Option<&ServerFailure> {
        match &self.0.cause {
            Cause::Remote(error) => Some(error),
            _ => None,
        }
    }
    pub fn http_status(&self) -> Option<u16> {
        match &self.0.cause {
            Cause::Response { status, .. } => Some(*status),
            _ => None,
        }
    }
    pub fn server_code(&self) -> Option<&str> {
        match &self.0.cause {
            Cause::Response { code, .. } => Some(code),
            Cause::Remote(error) => Some(error.code()),
            _ => None,
        }
    }
    pub fn server_message(&self) -> Option<&str> {
        match &self.0.cause {
            Cause::Response { message, .. } => Some(message),
            _ => None,
        }
    }
    pub fn request_id(&self) -> Option<&NotificationRequestId> {
        match &self.0.cause {
            Cause::Response { request_id, .. } => Some(request_id),
            Cause::Remote(error) => error.identity.request_id.as_ref(),
            _ => None,
        }
    }
    pub fn original_failure(&self) -> Option<&Self> {
        match &self.0.cause {
            Cause::Exhausted { original, .. } => Some(original),
            _ => None,
        }
    }
    pub fn last_attempt_failure(&self) -> Option<&Self> {
        match &self.0.cause {
            Cause::Exhausted { last, .. } => Some(last),
            _ => None,
        }
    }
    pub fn reconnect_attempts(&self) -> Option<u32> {
        match self.0.cause {
            Cause::Exhausted { attempts, .. } => Some(attempts),
            _ => None,
        }
    }
    pub(super) fn retry_after(&self) -> Option<Duration> {
        match self.0.cause {
            Cause::Response { retry_after, .. } => retry_after,
            _ => None,
        }
    }
    pub(super) fn retryable(&self) -> bool {
        self.0.retryable
    }
    fn new(kind: Kind, retryable: bool, cause: Cause) -> Self {
        Self(Arc::new(Failure {
            kind,
            retryable,
            cause,
        }))
    }
    pub(super) fn local(kind: Kind) -> Self {
        Self::new(kind, false, Cause::Local)
    }
    pub(crate) fn invalid_options() -> Self {
        Self::local(Kind::InvalidRequest)
    }
    pub(super) fn cancelled() -> Self {
        Self::local(Kind::Cancelled)
    }
    pub(super) fn timeout(stage: NotificationTimeoutStage) -> Self {
        Self::new(Kind::Timeout, true, Cause::Timeout(stage))
    }
    pub(super) fn protocol(error: ProtocolError) -> Self {
        let kind = match error {
            ProtocolError::UnexpectedEnd => Kind::Transport,
            ProtocolError::Allocation => Kind::Capacity,
            _ => Kind::Protocol,
        };
        Self::new(
            kind,
            error == ProtocolError::UnexpectedEnd,
            Cause::Protocol(error),
        )
    }
    pub(super) fn request(error: ProtocolError) -> Self {
        Self::new(
            if error == ProtocolError::Allocation {
                Kind::Capacity
            } else {
                Kind::InvalidRequest
            },
            false,
            Cause::Protocol(error),
        )
    }
    pub(super) fn transport(error: reqwest::Error) -> Self {
        let corrupt = corrupted_transport(&error);
        let retryable = !error.is_builder() && !corrupt;
        Self::new(
            if error.is_builder() {
                Kind::InvalidRequest
            } else if corrupt {
                Kind::Protocol
            } else if error.is_timeout() {
                Kind::Timeout
            } else {
                Kind::Transport
            },
            retryable,
            Cause::Transport(error.without_url()),
        )
    }
    pub(super) fn remote(error: ServerFailure) -> Self {
        let kind = error.known_kind().unwrap_or(Kind::Protocol);
        let retryable = error.retryable
            && matches!(
                kind,
                Kind::Capacity
                    | Kind::SourceUnavailable
                    | Kind::Transport
                    | Kind::Timeout
                    | Kind::ServerDraining
            );
        Self::new(kind, retryable, Cause::Remote(error))
    }
    pub(super) fn draining() -> Self {
        Self::new(Kind::ServerDraining, true, Cause::Local)
    }
    pub(super) fn http(
        status: u16,
        code: String,
        message: String,
        request_id: NotificationRequestId,
        retry_after: Option<Duration>,
    ) -> Self {
        let kind = match status {
            400 | 409 | 413 => Kind::InvalidRequest,
            401 => Kind::Authentication,
            403 => Kind::AuthorityRevoked,
            404 | 405 | 501 => Kind::Unsupported,
            429 => Kind::Capacity,
            503 => Kind::SourceUnavailable,
            _ => Kind::Protocol,
        };
        let retryable = matches!(
            (status, code.as_str()),
            (429, "NOTIFICATION_CAPACITY") | (503, "NOTIFICATION_SOURCE_UNAVAILABLE")
        );
        Self::new(
            kind,
            retryable,
            Cause::Response {
                status,
                code: code.into_boxed_str(),
                message: message.into_boxed_str(),
                request_id,
                retry_after,
            },
        )
    }
    pub(super) fn exhausted(original: Self, last: Self, attempts: u32) -> Self {
        Self::new(
            last.kind(),
            false,
            Cause::Exhausted {
                original,
                last,
                attempts,
            },
        )
    }
}

impl fmt::Display for HttpNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl fmt::Debug for HttpNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpNotificationError")
            .field("kind", &self.kind())
            .field("stage", &self.timeout_stage())
            .field("attempts", &self.reconnect_attempts())
            .finish_non_exhaustive()
    }
}

impl std::error::Error for HttpNotificationError {}

fn corrupted_transport(error: &reqwest::Error) -> bool {
    let mut cause = error.source();
    while let Some(error) = cause {
        if error
            .downcast_ref::<hyper::Error>()
            .is_some_and(hyper::Error::is_parse)
            || error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput
                )
            })
        {
            return true;
        }
        cause = error.source();
    }
    false
}
