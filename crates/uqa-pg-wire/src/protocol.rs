//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use thiserror::Error;

pub const PROTOCOL_VERSION_3_0: i32 = 196_608;
pub const PROTOCOL_VERSION_3_2: i32 = 196_610;
pub const CANCEL_REQUEST_CODE: i32 = 80_877_102;
pub const SSL_REQUEST_CODE: i32 = 80_877_103;
pub const GSSENC_REQUEST_CODE: i32 = 80_877_104;
/// Minimum cancellation key accepted in a `CancelRequest` packet.
pub const MIN_CANCEL_REQUEST_KEY_LEN: usize = 1;
/// Minimum cancellation key emitted in `BackendKeyData` under protocol 3.2.
pub const MIN_BACKEND_KEY_DATA_KEY_LEN: usize = 4;
pub const MAX_CANCEL_KEY_LEN: usize = 256;

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
    #[error(
        "invalid PostgreSQL cancellation key length {length}; expected {minimum} through {maximum} bytes"
    )]
    InvalidCancelKeyLength {
        length: usize,
        minimum: usize,
        maximum: usize,
    },
    #[error(
        "PostgreSQL protocol {major}.{minor} requires a 4-byte cancellation key, got {length} bytes"
    )]
    CancelKeyLengthForProtocol {
        length: usize,
        major: u16,
        minor: u16,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const V3_0: Self = Self { major: 3, minor: 0 };
    pub const V3_2: Self = Self { major: 3, minor: 2 };
    pub const LATEST: Self = Self::V3_2;

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

    /// Select the newest protocol version this crate supports without
    /// negotiating to a version newer than the frontend requested.
    pub fn negotiate(self) -> Result<Self, PgWireError> {
        if self.major != Self::LATEST.major {
            return Err(PgWireError::UnsupportedProtocolVersion(self.raw()));
        }
        Ok(Self {
            major: self.major,
            minor: self.minor.min(Self::LATEST.minor),
        })
    }
}

/// Opaque cancellation secret carried by `BackendKeyData` and
/// `CancelRequest`.
///
/// PostgreSQL 18 accepts 1 through 256 bytes when decoding a cancel request.
/// A backend key has the stricter 4 through 256 byte range in protocol 3.2,
/// and is exactly 4 bytes before protocol 3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelKey(Vec<u8>);

impl CancelKey {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, PgWireError> {
        let bytes = bytes.into();
        if !(MIN_CANCEL_REQUEST_KEY_LEN..=MAX_CANCEL_KEY_LEN).contains(&bytes.len()) {
            return Err(PgWireError::InvalidCancelKeyLength {
                length: bytes.len(),
                minimum: MIN_CANCEL_REQUEST_KEY_LEN,
                maximum: MAX_CANCEL_KEY_LEN,
            });
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn from_i32(secret_key: i32) -> Self {
        Self(secret_key.to_be_bytes().to_vec())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn validate_for_backend_key_data(
        &self,
        version: ProtocolVersion,
    ) -> Result<(), PgWireError> {
        let negotiated = version.negotiate()?;
        if negotiated < ProtocolVersion::V3_2 && self.0.len() != MIN_BACKEND_KEY_DATA_KEY_LEN {
            return Err(PgWireError::CancelKeyLengthForProtocol {
                length: self.0.len(),
                major: negotiated.major,
                minor: negotiated.minor,
            });
        }
        if self.0.len() < MIN_BACKEND_KEY_DATA_KEY_LEN {
            return Err(PgWireError::InvalidCancelKeyLength {
                length: self.0.len(),
                minimum: MIN_BACKEND_KEY_DATA_KEY_LEN,
                maximum: MAX_CANCEL_KEY_LEN,
            });
        }
        Ok(())
    }
}

impl From<i32> for CancelKey {
    fn from(secret_key: i32) -> Self {
        Self::from_i32(secret_key)
    }
}

impl AsRef<[u8]> for CancelKey {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
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
