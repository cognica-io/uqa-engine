//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use thiserror::Error;

pub const PROTOCOL_VERSION_3_0: i32 = 196_608;
pub const CANCEL_REQUEST_CODE: i32 = 80_877_102;
pub const SSL_REQUEST_CODE: i32 = 80_877_103;
pub const GSSENC_REQUEST_CODE: i32 = 80_877_104;

pub type DecodeOutcome<T> = Result<Option<(T, usize)>, PgWireError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PgWireError {
    #[error("invalid PostgreSQL wire message length {length}; minimum is {minimum}")]
    InvalidLength { length: i32, minimum: i32 },
    #[error("PostgreSQL wire message length {length} exceeds configured maximum {maximum}")]
    MessageTooLarge { length: i32, maximum: usize },
    #[error("invalid UTF-8 in {context}")]
    InvalidUtf8 { context: &'static str },
    #[error("missing nul terminator in {context}")]
    MissingNul { context: &'static str },
    #[error("trailing bytes in {context}: {remaining}")]
    TrailingBytes {
        context: &'static str,
        remaining: usize,
    },
    #[error("unexpected end of {context}")]
    UnexpectedEof { context: &'static str },
    #[error("unsupported PostgreSQL protocol version {0}")]
    UnsupportedProtocolVersion(i32),
    #[error("unknown frontend message tag {0:?}")]
    UnknownFrontendTag(u8),
    #[error("invalid format code {0}")]
    InvalidFormatCode(i16),
    #[error("invalid transaction status byte {0:?}")]
    InvalidTransactionStatus(u8),
    #[error("embedded nul byte in {context}")]
    EmbeddedNul { context: &'static str },
    #[error("invalid SQLSTATE {code:?}; expected exactly five ASCII letters or digits")]
    InvalidSqlState { code: String },
    #[error(
        "Bind parameter format count {format_count} must be zero, one, or match parameter count {parameter_count}"
    )]
    ParameterFormatCountMismatch {
        format_count: usize,
        parameter_count: usize,
    },
    #[error("{context} count {count} exceeds representable PostgreSQL i16")]
    CountTooLarge { context: &'static str, count: usize },
    #[error("{context} length {length} exceeds representable PostgreSQL i32")]
    LengthTooLarge {
        context: &'static str,
        length: usize,
    },
    #[error("{context} cannot be negative")]
    NegativeValue { context: &'static str },
}

pub type DecodeError = PgWireError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const V3_0: Self = Self { major: 3, minor: 0 };

    pub fn from_raw(raw: i32) -> Self {
        Self {
            major: ((raw >> 16) & 0xffff) as u16,
            minor: (raw & 0xffff) as u16,
        }
    }

    pub const fn raw(self) -> i32 {
        i32::from_be_bytes([
            (self.major >> 8) as u8,
            self.major as u8,
            (self.minor >> 8) as u8,
            self.minor as u8,
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatCode {
    Text,
    Binary,
}

impl FormatCode {
    pub fn from_i16(value: i16) -> Result<Self, PgWireError> {
        match value {
            0 => Ok(Self::Text),
            1 => Ok(Self::Binary),
            other => Err(PgWireError::InvalidFormatCode(other)),
        }
    }

    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Text => 0,
            Self::Binary => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle,
    InTransaction,
    Failed,
}

impl TransactionStatus {
    pub fn from_byte(value: u8) -> Result<Self, PgWireError> {
        match value {
            b'I' => Ok(Self::Idle),
            b'T' => Ok(Self::InTransaction),
            b'E' => Ok(Self::Failed),
            other => Err(PgWireError::InvalidTransactionStatus(other)),
        }
    }

    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Idle => b'I',
            Self::InTransaction => b'T',
            Self::Failed => b'E',
        }
    }
}
