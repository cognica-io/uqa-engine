//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared subtype identity for `PostgreSQL`'s built-in range families.

use serde::{Deserialize, Serialize};

use super::ColumnType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RangeSubtype {
    Integer,
    BigInteger,
    Numeric,
    Date,
    Timestamp,
    TimestampTz,
}

impl RangeSubtype {
    #[must_use]
    pub const fn range_name(self) -> &'static str {
        match self {
            Self::Integer => "int4range",
            Self::BigInteger => "int8range",
            Self::Numeric => "numrange",
            Self::Date => "daterange",
            Self::Timestamp => "tsrange",
            Self::TimestampTz => "tstzrange",
        }
    }

    #[must_use]
    pub const fn multirange_name(self) -> &'static str {
        match self {
            Self::Integer => "int4multirange",
            Self::BigInteger => "int8multirange",
            Self::Numeric => "nummultirange",
            Self::Date => "datemultirange",
            Self::Timestamp => "tsmultirange",
            Self::TimestampTz => "tstzmultirange",
        }
    }

    #[must_use]
    pub const fn scalar_type(self) -> ColumnType {
        match self {
            Self::Integer => ColumnType::Integer,
            Self::BigInteger => ColumnType::BigInteger,
            Self::Numeric => ColumnType::Numeric {
                precision: None,
                scale: None,
            },
            Self::Date => ColumnType::Date,
            Self::Timestamp => ColumnType::Timestamp,
            Self::TimestampTz => ColumnType::TimestampTz,
        }
    }
}
