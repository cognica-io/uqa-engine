//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded notification protocol values and incremental decoding. Transport establishment, listener authority and reconnection remain separate owners; decoding a ready frame does not establish those server-side facts.

mod decoder;
mod framing;
mod json;
mod request;
mod timing;
mod wire;

pub use decoder::{DecodeStep, NotificationDecoder};
pub use request::SubscriptionRequest;
pub use timing::{NotificationTiming, TimerLimits};
pub use wire::{NotificationReady, NotificationWireEvent, ServerFailure};

/// Complete request body and complete SSE frame limit, including event fields and delimiters.
pub const MAX_NOTIFICATION_WIRE_BYTES: usize = 65_536;

/// Content-free protocol failures. Rejected input and JSON diagnostic strings are never retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("notification protocol input exceeds its byte limit")]
    ByteLimit,
    #[error("notification protocol input is not valid UTF-8")]
    InvalidUTF8,
    #[error("notification protocol JSON is invalid or exceeds its nesting envelope")]
    InvalidJSON,
    #[error("notification protocol fields are invalid")]
    InvalidFields,
    #[error("notification protocol version is unsupported")]
    UnsupportedVersion,
    #[error("notification channel selection is invalid")]
    InvalidChannels,
    #[error("notification channel count exceeds its configured limit")]
    ChannelLimit,
    #[error("notification protocol does not support resumption")]
    ResumeUnsupported,
    #[error("notification response identity is invalid or changed")]
    Identity,
    #[error("notification event order is invalid")]
    EventOrder,
    #[error("notification sequence is invalid")]
    Sequence,
    #[error("notification payload exceeds its byte limit")]
    Payload,
    #[error("notification timing relationship is invalid")]
    Timing,
    #[error("notification timing exceeds the receiving runtime or local deadline")]
    TimerRange,
    #[error("notification protocol allocation failed")]
    Allocation,
    #[error("notification stream ended without a complete terminal frame")]
    UnexpectedEnd,
}

#[cfg(test)]
mod tests;
