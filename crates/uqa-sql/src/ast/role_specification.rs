//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL role keywords remain distinct from identically spelled quoted names.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum RoleSpecification {
    Named(String),
    CurrentUser,
    SessionUser,
}

impl From<String> for RoleSpecification {
    fn from(name: String) -> Self {
        Self::Named(name)
    }
}

impl From<&str> for RoleSpecification {
    fn from(name: &str) -> Self {
        Self::Named(name.into())
    }
}

impl std::fmt::Display for RoleSpecification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Named(name) => name,
            Self::CurrentUser => "CURRENT_USER",
            Self::SessionUser => "SESSION_USER",
        })
    }
}

impl<'de> Deserialize<'de> for RoleSpecification {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", content = "name", rename_all = "snake_case")]
        enum Tagged {
            Named(String),
            CurrentUser,
            SessionUser,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Tagged(Tagged),
            Legacy(String),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Tagged(Tagged::Named(name)) => Self::Named(name),
            Stored::Tagged(Tagged::CurrentUser) => Self::CurrentUser,
            Stored::Tagged(Tagged::SessionUser) => Self::SessionUser,
            // Preserve the meaning of old stored statements; newly compiled names always carry an explicit Named tag.
            Stored::Legacy(name) => match name.as_str() {
                "CURRENT_USER" => Self::CurrentUser,
                "SESSION_USER" => Self::SessionUser,
                _ => Self::Named(name),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_names_and_keywords_have_distinct_durable_encodings() {
        for specification in [
            RoleSpecification::Named("CURRENT_USER".into()),
            RoleSpecification::Named("SESSION_USER".into()),
            RoleSpecification::CurrentUser,
            RoleSpecification::SessionUser,
        ] {
            let stored = serde_json::to_string(&specification).unwrap();
            assert!(stored.starts_with('{'));
            assert_eq!(
                serde_json::from_str::<RoleSpecification>(&stored).unwrap(),
                specification
            );
        }
    }

    #[test]
    fn legacy_role_specifications_preserve_their_previous_keyword_meaning() {
        for (legacy, expected) in [
            ("CURRENT_USER", RoleSpecification::CurrentUser),
            ("SESSION_USER", RoleSpecification::SessionUser),
            ("reader", RoleSpecification::Named("reader".into())),
        ] {
            let json = serde_json::to_string(legacy).unwrap();
            assert_eq!(
                serde_json::from_str::<RoleSpecification>(&json).unwrap(),
                expected
            );
        }
    }
}
