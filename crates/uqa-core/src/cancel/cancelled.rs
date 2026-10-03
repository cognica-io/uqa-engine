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
    /// The session stayed idle in a transaction longer than its `idle_in_transaction_session_timeout` (`25P03`), which terminates it.
    IdleInTransactionSessionTimeout,
    /// The session stayed idle outside a transaction longer than its `idle_session_timeout` (`57P05`), which terminates it.
    IdleSessionTimeout,
    /// A transaction outlasted the session's `transaction_timeout` (`25P04`), which terminates the session.
    TransactionTimeout,
}

impl CancellationReason {
    /// The message `PostgreSQL` reports for a statement canceled for this reason.
    pub const fn message(self) -> &'static str {
        match self {
            Self::UserRequest => "canceling statement due to user request",
            Self::StatementTimeout => "canceling statement due to statement timeout",
            Self::LockTimeout => "canceling statement due to lock timeout",
            Self::IdleInTransactionSessionTimeout => {
                "terminating connection due to idle-in-transaction timeout"
            }
            Self::IdleSessionTimeout => "terminating connection due to idle-session timeout",
            Self::TransactionTimeout => "terminating connection due to transaction timeout",
        }
    }

    /// Whether the reason terminates the session, which `PostgreSQL` reports at `FATAL` and no exception handler catches.
    pub const fn terminates_session(self) -> bool {
        matches!(
            self,
            Self::IdleInTransactionSessionTimeout
                | Self::IdleSessionTimeout
                | Self::TransactionTimeout
        )
    }

    /// The SQLSTATE `PostgreSQL` reports: `57014` (`query_canceled`), or `55P03` (`lock_not_available`) for a lock timeout.
    pub const fn sqlstate(self) -> &'static str {
        match self {
            Self::UserRequest | Self::StatementTimeout => super::SQLSTATE_QUERY_CANCELED,
            Self::LockTimeout => "55P03",
            Self::IdleInTransactionSessionTimeout => "25P03",
            Self::IdleSessionTimeout => "57P05",
            Self::TransactionTimeout => "25P04",
        }
    }

    pub(super) const fn code(self) -> u8 {
        match self {
            Self::UserRequest => 1,
            Self::StatementTimeout => 2,
            Self::LockTimeout => 3,
            Self::IdleInTransactionSessionTimeout => 4,
            Self::IdleSessionTimeout => 5,
            Self::TransactionTimeout => 6,
        }
    }

    pub(super) const fn from_code(code: u8) -> Self {
        match code {
            2 => Self::StatementTimeout,
            3 => Self::LockTimeout,
            4 => Self::IdleInTransactionSessionTimeout,
            5 => Self::IdleSessionTimeout,
            6 => Self::TransactionTimeout,
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
