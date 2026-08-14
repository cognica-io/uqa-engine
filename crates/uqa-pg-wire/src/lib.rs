//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` wire protocol 3.0 through 3.2 primitives for UQA-compatible servers.
//!
//! This crate intentionally stops at protocol parsing and message encoding.
//! It does not own sockets, tasks, TLS, authentication storage, query
//! planning, or SQL execution. Integrators feed byte slices into the frontend
//! decoders and translate decoded protocol messages into their own server and
//! engine logic.
//!
//! Consequently this crate preserves cancellation requests, explicit error
//! responses, and failed-transaction status bytes, but it cannot decide when
//! an engine error or rollback must produce them. That mapping is a required
//! responsibility of the embedding server; there is no query-execution bridge
//! in this crate that could turn an execution failure into an empty success.

pub mod backend;
mod codec;
pub mod frontend;
pub mod protocol;

pub use backend::{
    encode_all, encode_all_for_protocol, Authentication, BackendKeyData, BackendMessage,
    CopyResponse, ErrorOrNotice, FieldDescription, GSSEncResponse, NoticeSeverity,
    NotificationResponse, SSLResponse,
};
pub use frontend::{
    decode_frontend, decode_frontend_with_max, decode_startup, decode_startup_with_max, Bind,
    CloseTarget, DescribeTarget, Execute, FrontendMessage, FunctionCall, Parse, StartupFrame,
    StartupMessage, StartupNegotiation,
};
pub use protocol::{
    CancelKey, DecodeError, DecodeOutcome, FormatCode, PgWireError, ProtocolVersion,
    TransactionStatus, CANCEL_REQUEST_CODE, GSSENC_REQUEST_CODE, MAX_CANCEL_KEY_LEN,
    MIN_BACKEND_KEY_DATA_KEY_LEN, MIN_CANCEL_REQUEST_KEY_LEN, PROTOCOL_VERSION_3_0,
    PROTOCOL_VERSION_3_2, SSL_REQUEST_CODE,
};
