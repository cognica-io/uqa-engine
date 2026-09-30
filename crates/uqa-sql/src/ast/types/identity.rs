//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID identities of user-defined types. Stored syntax, routine signatures and bound casts name a user-defined type by identity, so a rename, a schema move or a different search path cannot change which type they mean; output spells the type's current name. `#` cannot occur in an unquoted type name, and quoted names keep their quotes, so an identity never collides with a name as written.

use super::ColumnType;

const ENUM_PREFIX: &str = "enum#";
const DOMAIN_PREFIX: &str = "domain#";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTypeKind {
    Enum,
    Domain,
}

/// A parsed identity: the type's kind and OID, and the number of array dimensions around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTypeIdentity {
    pub kind: UserTypeKind,
    pub oid: u32,
    pub dimensions: usize,
}

impl UserTypeIdentity {
    /// Parse `enum#<oid>` or `domain#<oid>` with any number of `[]` suffixes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let mut element = name.trim();
        let mut dimensions = 0usize;
        while let Some(inner) = element.strip_suffix("[]") {
            element = inner;
            dimensions += 1;
        }
        let (kind, oid) = if let Some(oid) = element.strip_prefix(ENUM_PREFIX) {
            (UserTypeKind::Enum, oid)
        } else {
            (UserTypeKind::Domain, element.strip_prefix(DOMAIN_PREFIX)?)
        };
        if oid.is_empty() || !oid.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            kind,
            oid: oid.parse().ok()?,
            dimensions,
        })
    }
}

impl ColumnType {
    /// The identity of a user-defined type or an array of one; built-in types have none.
    #[must_use]
    pub fn user_type_identity(&self) -> Option<String> {
        match self {
            ColumnType::Enum(reference) => Some(format!("{ENUM_PREFIX}{}", reference.oid)),
            ColumnType::Domain { oid, .. } => Some(format!("{DOMAIN_PREFIX}{oid}")),
            ColumnType::Array(element) => element
                .user_type_identity()
                .map(|element| format!("{element}[]")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UserTypeIdentity, UserTypeKind};
    use crate::ast::{ColumnType, EnumTypeReference};

    #[test]
    fn identities_round_trip_through_arrays() {
        let mood = ColumnType::Enum(EnumTypeReference {
            schema: "public".into(),
            name: "mood".into(),
            oid: 20_000,
            array_oid: 20_001,
        });
        let array = ColumnType::Array(Box::new(ColumnType::Array(Box::new(mood))));
        let identity = array.user_type_identity().unwrap();
        assert_eq!(identity, "enum#20000[][]");
        assert_eq!(
            UserTypeIdentity::parse(&identity),
            Some(UserTypeIdentity {
                kind: UserTypeKind::Enum,
                oid: 20_000,
                dimensions: 2,
            })
        );
        assert_eq!(
            UserTypeIdentity::parse("domain#7"),
            Some(UserTypeIdentity {
                kind: UserTypeKind::Domain,
                oid: 7,
                dimensions: 0,
            })
        );
        for name in ["\"enum#1\"", "enum#", "enum#x", "domain#-1", "integer"] {
            assert_eq!(UserTypeIdentity::parse(name), None, "{name}");
        }
        assert_eq!(ColumnType::Integer.user_type_identity(), None);
    }
}
