//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` v3 wire protocol primitives for UQA-compatible servers.
//!
//! This crate intentionally stops at protocol parsing and message encoding.
//! It does not own sockets, tasks, TLS, authentication storage, query
//! planning, or SQL execution. Integrators feed byte slices into the frontend
//! decoders and translate decoded protocol messages into their own server and
//! engine logic.

pub mod backend;
mod codec;
pub mod frontend;
pub mod protocol;

pub use backend::{
    encode_all, Authentication, BackendKeyData, BackendMessage, CopyResponse, ErrorOrNotice,
    FieldDescription, GSSEncResponse, NoticeSeverity, SSLResponse,
};
pub use frontend::{
    decode_frontend, decode_frontend_with_max, decode_startup, decode_startup_with_max, Bind,
    CloseTarget, DescribeTarget, Execute, FrontendMessage, Parse, StartupFrame, StartupMessage,
};
pub use protocol::{
    DecodeError, DecodeOutcome, FormatCode, PgWireError, ProtocolVersion, TransactionStatus,
    CANCEL_REQUEST_CODE, GSSENC_REQUEST_CODE, PROTOCOL_VERSION_3_0, SSL_REQUEST_CODE,
};
