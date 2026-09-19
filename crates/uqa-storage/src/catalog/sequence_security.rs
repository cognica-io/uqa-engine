//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A durable discriminator prevents current sequence authority from falling back to names.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
pub use uqa_core::catalog_sequence::{BoundSequenceSecurity, LegacySequenceSecurity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceSecurityRow {
    Bound(BoundSequenceSecurity),
    Legacy(LegacySequenceSecurity),
}

impl SequenceSecurityRow {
    pub fn bootstrap() -> Self {
        Self::Bound(BoundSequenceSecurity::owner(
            uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        ))
    }

    pub fn legacy(role_owner: impl Into<String>) -> Self {
        Self::Legacy(LegacySequenceSecurity::owner(role_owner))
    }
}

impl From<BoundSequenceSecurity> for SequenceSecurityRow {
    fn from(row: BoundSequenceSecurity) -> Self {
        Self::Bound(row)
    }
}

impl Serialize for SequenceSecurityRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Bound<'a> {
            sequence_security_format: u32,
            #[serde(flatten)]
            security: &'a BoundSequenceSecurity,
        }
        match self {
            Self::Bound(security) => Bound {
                sequence_security_format: 1,
                security,
            }
            .serialize(serializer),
            Self::Legacy(security) => security.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SequenceSecurityRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let Some(version) = value.get("sequence_security_format") {
            if version.as_u64() != Some(1) {
                return Err(D::Error::custom("unsupported sequence security format"));
            }
            value
                .as_object_mut()
                .expect("version belongs to an object")
                .remove("sequence_security_format");
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
