//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native range families and catalog-bound identities of declared ranges.

use serde::{Deserialize, Serialize};

use super::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

#[cfg(test)]
mod tests;

/// Physical range containers require at least integer alignment, even when their endpoint is narrower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeAlignment {
    Integer,
    Double,
}

impl RangeAlignment {
    #[must_use]
    pub fn for_endpoint(endpoint: &ColumnType) -> Self {
        if crate::catalog::type_metadata::pg_type_align(endpoint) == "d" {
            Self::Double
        } else {
            Self::Integer
        }
    }

    #[must_use]
    pub const fn catalog_name(self) -> &'static str {
        match self {
            Self::Integer => "i",
            Self::Double => "d",
        }
    }
}

/// Current names and physical layout of one range or multirange type. Identity equality does not depend on a rename or on the endpoint's type identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeTypeReference {
    pub schema: String,
    pub name: String,
    pub oid: u32,
    pub array_oid: u32,
    pub alignment: RangeAlignment,
}

impl PartialEq for RangeTypeReference {
    fn eq(&self, other: &Self) -> bool {
        self.oid == other.oid
    }
}

impl Eq for RangeTypeReference {}

impl RangeTypeReference {
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let schema = control.copy_text(&self.schema)?;
        let name = control.copy_text(&self.name)?;
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        control.finish(
            Self {
                schema,
                name,
                oid: self.oid,
                array_oid: self.array_oid,
                alignment: self.alignment,
            },
            control.combine(schema_memory, name_memory),
        )
    }
}

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
