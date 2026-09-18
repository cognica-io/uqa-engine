//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uqa_core::catalog_schema::SchemaRow as LegacySchemaRow;
pub use uqa_core::catalog_schema::{BoundSchemaRow, SchemaAclEntry, SchemaPrivileges};

/// A catalog read distinguishes old names from already captured role identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaRow {
    Bound(BoundSchemaRow),
    Legacy(LegacySchemaRow),
}

impl SchemaRow {
    pub fn name(&self) -> &str {
        match self {
            Self::Bound(row) => &row.name,
            Self::Legacy(row) => &row.name,
        }
    }

    pub fn legacy(name: impl Into<String>) -> Self {
        Self::Legacy(LegacySchemaRow::legacy(name))
    }

    pub fn bootstrap(name: impl Into<String>) -> Self {
        Self::Bound(BoundSchemaRow::bootstrap(name))
    }
}

impl From<BoundSchemaRow> for SchemaRow {
    fn from(row: BoundSchemaRow) -> Self {
        Self::Bound(row)
    }
}

impl Serialize for SchemaRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Bound<'a> {
            schema_security_format: u32,
            #[serde(flatten)]
            row: &'a BoundSchemaRow,
        }
        match self {
            Self::Legacy(row) => row.serialize(serializer),
            Self::Bound(row) => Bound {
                schema_security_format: 1,
                row,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SchemaRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(version) = value.get("schema_security_format") {
            if version.as_u64() != Some(1) {
                return Err(D::Error::custom("unsupported schema security format"));
            }
            serde_json::from_value(value)
                .map(Self::Bound)
                .map_err(D::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Legacy)
                .map_err(D::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests;
