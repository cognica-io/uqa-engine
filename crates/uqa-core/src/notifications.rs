//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Notification values shared by embedded listeners and remote clients.
//!
//! These types do not register listeners, encode a wire protocol, or establish delivery. A producer must preserve its effective registration boundary and emit contiguous, positive sequences within each epoch. A replacement epoch does not imply replay.

mod identity;

pub use identity::{InvalidNotificationIdentity, NotificationEpoch, NotificationRequestId};

/// One committed SQL notification waiting for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLNotification {
    /// Stable backend process identifier of the sending SQL session.
    pub process_id: i32,
    /// Subscribed SQL channel that received the message.
    pub channel: String,
    /// Sender-provided payload, or the empty string when `NOTIFY` omitted it.
    pub payload: String,
}

/// The identity of one ready subscription, independent of its delivery adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationIdentity {
    /// Fresh identity for this registration; it is not a durable cursor.
    pub epoch: NotificationEpoch,
    /// Present only when delivery has an HTTP request identity.
    pub request_id: Option<NotificationRequestId>,
}

/// A closed failure category shared by language bindings and delivery adapters.
///
/// Adapters retain the original cause separately. A category does not grant retry authority: authentication, protocol and backpressure failures are terminal by default, and transport retries require the caller's remaining reconnect budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationFailureKind {
    InvalidRequest,
    Authentication,
    AuthorityRevoked,
    Unsupported,
    Capacity,
    Backpressure,
    Protocol,
    SourceUnavailable,
    Transport,
    Timeout,
    ServerDraining,
    Cancelled,
    SequenceExhausted,
}

impl NotificationFailureKind {
    /// Stable, content-free category for cross-language errors and diagnostics.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "NOTIFICATION_INVALID_REQUEST",
            Self::Authentication => "NOTIFICATION_AUTHENTICATION",
            Self::AuthorityRevoked => "NOTIFICATION_AUTHORITY_REVOKED",
            Self::Unsupported => "NOTIFICATION_UNSUPPORTED",
            Self::Capacity => "NOTIFICATION_CAPACITY",
            Self::Backpressure => "NOTIFICATION_BACKPRESSURE",
            Self::Protocol => "NOTIFICATION_PROTOCOL",
            Self::SourceUnavailable => "NOTIFICATION_SOURCE_UNAVAILABLE",
            Self::Transport => "NOTIFICATION_TRANSPORT",
            Self::Timeout => "NOTIFICATION_TIMEOUT",
            Self::ServerDraining => "NOTIFICATION_SERVER_DRAINING",
            Self::Cancelled => "NOTIFICATION_CANCELLED",
            Self::SequenceExhausted => "NOTIFICATION_SEQUENCE_EXHAUSTED",
        }
    }
}

/// An ordered subscription observation, including any loss of continuity.
///
/// Readiness precedes the first event. `ResyncRequired` names the old identity and precedes replacement data; `Reconnected` names the new ready identity. Embedded listeners do not reconnect implicitly. Channel and payload content is omitted from `Debug`; applications access it explicitly through the notification value.
#[derive(Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    Notification {
        identity: NotificationIdentity,
        /// Positive, contiguous counter within `identity.epoch`, starting at one. JavaScript adapters must preserve this exact integer as `bigint`.
        sequence: u64,
        notification: SQLNotification,
    },
    ResyncRequired {
        identity: NotificationIdentity,
        cause: NotificationFailureKind,
    },
    Reconnected {
        identity: NotificationIdentity,
    },
}

impl std::fmt::Debug for NotificationEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notification {
                identity, sequence, ..
            } => f
                .debug_struct("Notification")
                .field("identity", identity)
                .field("sequence", sequence)
                .finish_non_exhaustive(),
            Self::ResyncRequired { identity, cause } => f
                .debug_struct("ResyncRequired")
                .field("identity", identity)
                .field("cause", cause)
                .finish(),
            Self::Reconnected { identity } => f
                .debug_struct("Reconnected")
                .field("identity", identity)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests;
