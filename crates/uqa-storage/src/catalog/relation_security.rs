//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The stored discriminator separates captured role identities from legacy names.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
pub use uqa_core::catalog_acl::{BoundRelationSecurity, LegacyRelationSecurity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationSecurityRow {
    Bound(BoundRelationSecurity),
    Legacy(LegacyRelationSecurity),
}

impl RelationSecurityRow {
    pub fn bootstrap() -> Self {
        Self::Bound(BoundRelationSecurity::owner(
            uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        ))
    }

    pub fn legacy(role_owner: impl Into<String>) -> Self {
        Self::Legacy(LegacyRelationSecurity::owner(role_owner))
    }
}

impl From<BoundRelationSecurity> for RelationSecurityRow {
    fn from(row: BoundRelationSecurity) -> Self {
        Self::Bound(row)
    }
}

impl Serialize for RelationSecurityRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Bound<'a> {
            relation_security_format: u32,
            #[serde(flatten)]
            security: &'a BoundRelationSecurity,
        }
        match self {
            Self::Bound(security) => Bound {
                relation_security_format: 1,
                security,
            }
            .serialize(serializer),
            Self::Legacy(security) => security.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RelationSecurityRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut value = serde_json::Value::deserialize(deserializer)?;
        if let Some(version) = value.get("relation_security_format") {
            if version.as_u64() != Some(1) {
                return Err(D::Error::custom("unsupported relation security format"));
            }
            value
                .as_object_mut()
                .expect("version belongs to an object")
                .remove("relation_security_format");
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
