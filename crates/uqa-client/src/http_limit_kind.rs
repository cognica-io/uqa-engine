//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// Closed numeric boundary carried by a rejected HTTP request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpLimitKind {
    RequestBodyBytes,
    #[serde(rename = "sql_bytes")]
    SQLBytes,
    Parameters,
    BatchStatements,
    ResultBytes,
    QueryAdmission,
    AuthenticationAdmission,
}

impl HttpLimitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestBodyBytes => "request_body_bytes",
            Self::SQLBytes => "sql_bytes",
            Self::Parameters => "parameters",
            Self::BatchStatements => "batch_statements",
            Self::ResultBytes => "result_bytes",
            Self::QueryAdmission => "query_admission",
            Self::AuthenticationAdmission => "authentication_admission",
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::RequestBodyBytes => "REQUEST_BODY_TOO_LARGE",
            Self::SQLBytes => "SQL_TOO_LARGE",
            Self::Parameters => "TOO_MANY_PARAMETERS",
            Self::BatchStatements => "TOO_MANY_STATEMENTS",
            Self::ResultBytes => "RESULT_TOO_LARGE",
            Self::QueryAdmission | Self::AuthenticationAdmission => "NODE_BUSY",
        }
    }

    pub(crate) const fn is_admission(self) -> bool {
        matches!(self, Self::QueryAdmission | Self::AuthenticationAdmission)
    }
}
