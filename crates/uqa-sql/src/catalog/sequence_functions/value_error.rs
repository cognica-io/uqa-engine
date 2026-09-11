//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable diagnostics and SQLSTATEs for sequence value functions.
use crate::SQLError;
#[derive(Debug, thiserror::Error)]
pub enum SequenceValueError {
    #[error("relation \"{0}\" does not exist")]
    Undefined(String),
    #[error("cannot open relation \"{name}\": this operation is not supported for {kind}s")]
    WrongKind { name: String, kind: &'static str },
    #[error("currval of sequence \"{0}\" is not yet defined in this session")]
    CurrvalUndefined(String),
    #[error("lastval is not yet defined in this session")]
    LastvalUndefined,
    #[error("setval: value {value} is out of bounds for sequence \"{name}\" ({min}..{max})")]
    SetvalOutOfBounds {
        name: String,
        value: i64,
        min: i64,
        max: i64,
    },
    #[error("nextval: reached {bound} value of sequence \"{name}\" ({value})")]
    Exhausted {
        name: String,
        bound: &'static str,
        value: i64,
    },
    #[error("cannot execute {0}() in a read-only transaction")]
    ReadOnly(&'static str),
    #[error(transparent)]
    Security(#[from] SQLError),
    #[error("{0}")]
    Internal(String),
}

impl SequenceValueError {
    pub fn into_sql_error(self) -> SQLError {
        let sqlstate = match self {
            Self::Undefined(_) => "42P01",
            Self::WrongKind { .. } => "42809",
            Self::CurrvalUndefined(_) | Self::LastvalUndefined => "55000",
            Self::SetvalOutOfBounds { .. } => "22003",
            Self::Exhausted { .. } => "2200H",
            Self::ReadOnly(_) => "25006",
            Self::Security(error) => return error,
            Self::Internal(message) => return SQLError::Internal(message),
        };
        SQLError::Routine {
            sqlstate: sqlstate.into(),
            message: self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
