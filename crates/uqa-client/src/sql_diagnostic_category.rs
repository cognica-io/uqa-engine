//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// Closed SQL failure categories; never derived from an error message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SQLDiagnosticCategory {
    Syntax,
    UndefinedTable,
    UndefinedColumn,
    AmbiguousColumn,
    UndefinedFunction,
    AmbiguousFunction,
    TypeMismatch,
    InvalidParameter,
    VectorDimensionMismatch,
    IndexRequired,
    UndefinedObject,
    DuplicateObject,
    ConstraintViolation,
    Unsupported,
    Cancelled,
    ResourceExhausted,
    Transaction,
    Permission,
    Internal,
    Other,
}

impl SQLDiagnosticCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::UndefinedTable => "undefined_table",
            Self::UndefinedColumn => "undefined_column",
            Self::AmbiguousColumn => "ambiguous_column",
            Self::UndefinedFunction => "undefined_function",
            Self::AmbiguousFunction => "ambiguous_function",
            Self::TypeMismatch => "type_mismatch",
            Self::InvalidParameter => "invalid_parameter",
            Self::VectorDimensionMismatch => "vector_dimension_mismatch",
            Self::IndexRequired => "index_required",
            Self::UndefinedObject => "undefined_object",
            Self::DuplicateObject => "duplicate_object",
            Self::ConstraintViolation => "constraint_violation",
            Self::Unsupported => "unsupported",
            Self::Cancelled => "cancelled",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Transaction => "transaction",
            Self::Permission => "permission",
            Self::Internal => "internal",
            Self::Other => "other",
        }
    }

    /// Source-owned guidance, independent of remote or customer-controlled text.
    pub const fn hint(self) -> Option<&'static str> {
        match self {
            Self::Syntax => Some("Check SQL syntax at the reported character position."),
            Self::UndefinedTable => Some("Check the relation name and schema search path."),
            Self::UndefinedColumn => Some("Check column names against the relation schema."),
            Self::AmbiguousColumn => Some("Qualify the column with its relation alias."),
            Self::UndefinedFunction | Self::AmbiguousFunction | Self::TypeMismatch => {
                Some("Check argument types and explicit casts.")
            }
            Self::VectorDimensionMismatch => {
                Some("Match the vector dimensions to the declared type.")
            }
            Self::IndexRequired => Some("Create a GIN text index on the searched field."),
            _ => None,
        }
    }
}
