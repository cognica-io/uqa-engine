//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PUBLIC and a named ACL recipient have distinct durable representations.

use serde::{Deserialize, Deserializer, Serialize};

/// A named recipient can have any valid role spelling, including the quoted name `PUBLIC`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum AclGrantee {
    Public,
    Role(String),
}

impl AclGrantee {
    #[must_use]
    pub fn role_name(&self) -> Option<&str> {
        match self {
            Self::Public => None,
            Self::Role(name) => Some(name),
        }
    }

    #[must_use]
    pub const fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }
}

impl From<String> for AclGrantee {
    fn from(name: String) -> Self {
        Self::Role(name)
    }
}

impl From<&str> for AclGrantee {
    fn from(name: &str) -> Self {
        Self::Role(name.into())
    }
}

impl std::fmt::Display for AclGrantee {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.role_name().unwrap_or("PUBLIC"))
    }
}

impl<'de> Deserialize<'de> for AclGrantee {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "name",
            rename_all = "snake_case",
            deny_unknown_fields
        )]
        enum Tagged {
            Public,
            Role(String),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Tagged(Tagged),
            Legacy(String),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Tagged(Tagged::Public) => Self::Public,
            // Legacy ACLs encoded the PUBLIC recipient as this exact string. No role-catalog lookup can recover a different original intent from those bytes.
            Stored::Legacy(name) if name == "PUBLIC" => Self::Public,
            Stored::Tagged(Tagged::Role(name)) | Stored::Legacy(name) => Self::Role(name),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_and_keyword_spelled_named_recipients_round_trip_separately() {
        let grantees = [
            AclGrantee::Public,
            AclGrantee::Role("PUBLIC".into()),
            AclGrantee::Role("CURRENT_USER".into()),
            AclGrantee::Role("SESSION_USER".into()),
            AclGrantee::Role("reader".into()),
        ];
        let mut encoded = std::collections::BTreeSet::new();
        for grantee in grantees {
            let json = serde_json::to_string(&grantee).unwrap();
            assert!(encoded.insert(json.clone()));
            assert_eq!(serde_json::from_str::<AclGrantee>(&json).unwrap(), grantee);
        }
        assert!(!AclGrantee::from("PUBLIC").is_public());
        assert_eq!(AclGrantee::Public.role_name(), None);
    }

    #[test]
    fn legacy_acl_strings_preserve_their_stored_grantee_meaning() {
        for (name, expected) in [
            ("PUBLIC", AclGrantee::Public),
            ("CURRENT_USER", AclGrantee::Role("CURRENT_USER".into())),
            ("SESSION_USER", AclGrantee::Role("SESSION_USER".into())),
            ("reader", AclGrantee::Role("reader".into())),
        ] {
            let json = serde_json::to_string(name).unwrap();
            assert_eq!(serde_json::from_str::<AclGrantee>(&json).unwrap(), expected);
        }
    }

    #[test]
    fn malformed_current_grantees_never_become_public_or_legacy_names() {
        for json in [
            r#"{"kind":"unknown","name":"PUBLIC"}"#,
            r#"{"kind":"role"}"#,
            r#"{"kind":"role","name":null}"#,
            r#"{"kind":null,"name":"PUBLIC"}"#,
            r#"{"name":"PUBLIC"}"#,
            r#"{"kind":"public","name":"reader"}"#,
            "null",
            "42",
        ] {
            assert!(serde_json::from_str::<AclGrantee>(json).is_err(), "{json}");
        }
    }
}
