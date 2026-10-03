//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The error of a canceled statement and why it was canceled.

use thiserror::Error;

/// Why a statement was canceled, which decides the message and SQLSTATE `PostgreSQL` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancellationReason {
    /// A client's cancel request or [`super::CancellationToken::cancel`] (`57014`).
    #[default]
    UserRequest,
    /// The session's `statement_timeout` elapsed (`57014`).
    StatementTimeout,
    /// A lock wait outlasted the session's `lock_timeout` (`55P03`).
    LockTimeout,
}

impl CancellationReason {
    /// The message `PostgreSQL` reports for a statement canceled for this reason.
    pub const fn message(self) -> &'static str {
        match self {
            Self::UserRequest => "canceling statement due to user request",
            Self::StatementTimeout => "canceling statement due to statement timeout",
            Self::LockTimeout => "canceling statement due to lock timeout",
        }
    }

    /// The SQLSTATE `PostgreSQL` reports: `57014` (`query_canceled`), or `55P03` (`lock_not_available`) for a lock timeout.
    pub const fn sqlstate(self) -> &'static str {
        match self {
            Self::UserRequest | Self::StatementTimeout => super::SQLSTATE_QUERY_CANCELED,
            Self::LockTimeout => "55P03",
        }
    }

    pub(super) const fn code(self) -> u8 {
        match self {
            Self::UserRequest => 1,
            Self::StatementTimeout => 2,
            Self::LockTimeout => 3,
        }
    }

    pub(super) const fn from_code(code: u8) -> Self {
        match code {
            2 => Self::StatementTimeout,
            3 => Self::LockTimeout,
            _ => Self::UserRequest,
        }
    }
}

/// Raised when a statement is canceled; its `Display` payload is the message `PostgreSQL` reports for the reason.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq, Default)]
#[error("{}", .reason.message())]
pub struct QueryCancelled {
    pub reason: CancellationReason,
}

impl QueryCancelled {
    /// A cancellation a client requested.
    pub const USER_REQUEST: Self = Self::new(CancellationReason::UserRequest);

    pub const fn new(reason: CancellationReason) -> Self {
        Self { reason }
    }

    pub const fn sqlstate(&self) -> &'static str {
        self.reason.sqlstate()
    }
}
