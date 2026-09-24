//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Object-privilege recipients preserve PUBLIC separately from named and session roles.

use super::RoleSpecification;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "role", rename_all = "snake_case")]
pub enum AclRoleSpecification {
    Public,
    Role(RoleSpecification),
}

impl From<RoleSpecification> for AclRoleSpecification {
    fn from(role: RoleSpecification) -> Self {
        Self::Role(role)
    }
}

impl From<String> for AclRoleSpecification {
    fn from(name: String) -> Self {
        Self::Role(RoleSpecification::Named(name))
    }
}

impl From<&str> for AclRoleSpecification {
    fn from(name: &str) -> Self {
        Self::from(name.to_owned())
    }
}

impl<'de> Deserialize<'de> for AclRoleSpecification {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "role",
            rename_all = "snake_case",
            deny_unknown_fields
        )]
        enum Tagged {
            Public,
            Role(RoleSpecification),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Tagged(Tagged),
            Legacy(String),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Tagged(Tagged::Public) => Self::Public,
            Stored::Tagged(Tagged::Role(role)) => Self::Role(role),
            Stored::Legacy(name) => match name.as_str() {
                "PUBLIC" => Self::Public,
                "CURRENT_USER" => Self::Role(RoleSpecification::CurrentUser),
                "SESSION_USER" => Self::Role(RoleSpecification::SessionUser),
                _ => Self::Role(RoleSpecification::Named(name)),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_role_names_public_and_session_keywords_remain_distinct() {
        let specifications = [
            AclRoleSpecification::Public,
            AclRoleSpecification::from("PUBLIC"),
            AclRoleSpecification::from("CURRENT_USER"),
            AclRoleSpecification::from("SESSION_USER"),
            AclRoleSpecification::from(RoleSpecification::CurrentUser),
            AclRoleSpecification::from(RoleSpecification::SessionUser),
        ];
        let mut encodings = std::collections::BTreeSet::new();
        for specification in specifications {
            let json = serde_json::to_string(&specification).unwrap();
            assert!(encodings.insert(json.clone()));
            assert_eq!(
                serde_json::from_str::<AclRoleSpecification>(&json).unwrap(),
                specification
            );
        }
    }

    #[test]
    fn legacy_object_grant_statements_keep_their_previous_keyword_meanings() {
        for (name, expected) in [
            ("PUBLIC", AclRoleSpecification::Public),
            ("CURRENT_USER", RoleSpecification::CurrentUser.into()),
            ("SESSION_USER", RoleSpecification::SessionUser.into()),
            ("reader", AclRoleSpecification::from("reader")),
        ] {
            let json = serde_json::to_string(name).unwrap();
            assert_eq!(
                serde_json::from_str::<AclRoleSpecification>(&json).unwrap(),
                expected
            );
        }
        for json in [
            r#"{"kind":"unknown","role":"PUBLIC"}"#,
            r#"{"kind":"role"}"#,
            r#"{"kind":"public","role":"reader"}"#,
            r#"{"role":"PUBLIC"}"#,
            r#"{"kind":"role","role":{"kind":"named","name":"PUBLIC","extra":true}}"#,
            r#"{"kind":"role","role":{"kind":"current_user","name":"PUBLIC"}}"#,
            "null",
        ] {
            assert!(
                serde_json::from_str::<AclRoleSpecification>(json).is_err(),
                "{json}"
            );
        }
    }
}
