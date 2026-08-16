//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;

use reqwest::StatusCode;
use thiserror::Error;

/// Redacted failure returned by [`crate::HttpEngine`].
#[derive(Error)]
pub enum HttpEngineError {
    #[error("UQA data-plane URL is invalid")]
    InvalidBaseURL,
    #[error("plain HTTP UQA URLs must resolve to loopback")]
    InsecureRemoteURL,
    #[error("UQA project token must not be empty")]
    InvalidCredential,
    #[error("required UQA connection environment variable {0} is missing")]
    MissingEnvironmentVariable(&'static str),
    #[error("SQL text must not be empty")]
    EmptySQL,
    #[error("SQL parameter cannot be represented by the HTTP protocol")]
    InvalidParameter,
    #[error("UQA HTTP client could not be initialized")]
    BuildClient(#[source] reqwest::Error),
    #[error("UQA HTTP transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("UQA returned {status} with code {code}")]
    Server {
        status: StatusCode,
        code: String,
        message: String,
        request_id: Option<String>,
    },
    #[error("UQA response exceeded the client safety limit")]
    ResponseTooLarge,
    #[error("UQA response content type is invalid")]
    UnexpectedContentType,
    #[error("UQA response is missing its request ID")]
    MissingRequestId,
    #[error("UQA response request IDs do not match")]
    ResponseRequestIdMismatch,
    #[error("UQA response body is not valid JSON")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("UQA NDJSON stream frame exceeded the client safety limit")]
    StreamFrameTooLarge,
    #[error("UQA NDJSON stream frame order is invalid")]
    InvalidStreamSequence,
    #[error("UQA NDJSON stream ended before a terminal frame")]
    TruncatedStream,
    #[error("UQA NDJSON stream request ID does not match its HTTP response")]
    StreamRequestIdMismatch,
}

impl HttpEngineError {
    pub(crate) fn build_client(error: reqwest::Error) -> Self {
        Self::BuildClient(error.without_url())
    }

    pub(crate) fn transport(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}

impl fmt::Debug for HttpEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server {
                status,
                code,
                request_id,
                ..
            } => formatter
                .debug_struct("HttpEngineError::Server")
                .field("status", status)
                .field("code", code)
                .field("request_id", request_id)
                .finish(),
            Self::InvalidBaseURL => formatter.write_str("HttpEngineError::InvalidBaseURL"),
            Self::InsecureRemoteURL => formatter.write_str("HttpEngineError::InsecureRemoteURL"),
            Self::InvalidCredential => formatter.write_str("HttpEngineError::InvalidCredential"),
            Self::MissingEnvironmentVariable(name) => formatter
                .debug_tuple("HttpEngineError::MissingEnvironmentVariable")
                .field(name)
                .finish(),
            Self::EmptySQL => formatter.write_str("HttpEngineError::EmptySQL"),
            Self::InvalidParameter => formatter.write_str("HttpEngineError::InvalidParameter"),
            Self::BuildClient(_) => formatter.write_str("HttpEngineError::BuildClient([REDACTED])"),
            Self::Transport(_) => formatter.write_str("HttpEngineError::Transport([REDACTED])"),
            Self::ResponseTooLarge => formatter.write_str("HttpEngineError::ResponseTooLarge"),
            Self::UnexpectedContentType => {
                formatter.write_str("HttpEngineError::UnexpectedContentType")
            }
            Self::MissingRequestId => formatter.write_str("HttpEngineError::MissingRequestId"),
            Self::ResponseRequestIdMismatch => {
                formatter.write_str("HttpEngineError::ResponseRequestIdMismatch")
            }
            Self::InvalidResponse(_) => {
                formatter.write_str("HttpEngineError::InvalidResponse([REDACTED])")
            }
            Self::StreamFrameTooLarge => {
                formatter.write_str("HttpEngineError::StreamFrameTooLarge")
            }
            Self::InvalidStreamSequence => {
                formatter.write_str("HttpEngineError::InvalidStreamSequence")
            }
            Self::TruncatedStream => formatter.write_str("HttpEngineError::TruncatedStream"),
            Self::StreamRequestIdMismatch => {
                formatter.write_str("HttpEngineError::StreamRequestIdMismatch")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_debug_output_omits_customer_message() {
        let secret = "customer SQL and result";
        let error = HttpEngineError::Server {
            status: StatusCode::BAD_REQUEST,
            code: "SQL_EXECUTION_FAILED".to_owned(),
            message: secret.to_owned(),
            request_id: Some("qry_test".to_owned()),
        };

        let debug = format!("{error:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("SQL_EXECUTION_FAILED"));
    }
}
