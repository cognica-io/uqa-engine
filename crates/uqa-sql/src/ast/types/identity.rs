//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID identities of user-defined types. Stored syntax, routine signatures and bound casts name a user-defined type by identity, so a rename, a schema move or a different search path cannot change which type they mean; output spells the type's current name. `#` cannot occur in an unquoted type name, and quoted names keep their quotes, so an identity never collides with a name as written.

use super::ColumnType;
mod production;

const ENUM_PREFIX: &str = "enum#";
const DOMAIN_PREFIX: &str = "domain#";
const COMPOSITE_PREFIX: &str = "composite#";
const RANGE_PREFIX: &str = "range#";
const MULTIRANGE_PREFIX: &str = "multirange#";

pub(super) fn write_identity(ty: &ColumnType, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match ty {
        ColumnType::Enum(reference) => write!(f, "{ENUM_PREFIX}{}", reference.oid),
        ColumnType::Composite(reference) => write!(f, "{COMPOSITE_PREFIX}{}", reference.oid),
        ColumnType::DeclaredRange(reference) => write!(f, "{RANGE_PREFIX}{}", reference.oid),
        ColumnType::DeclaredMultirange(reference) => {
            write!(f, "{MULTIRANGE_PREFIX}{}", reference.oid)
        }
        ColumnType::Domain { oid, .. } => write!(f, "{DOMAIN_PREFIX}{oid}"),
        _ => unreachable!("catalog identity formatter requires a user-defined scalar type"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTypeKind {
    Enum,
    Domain,
    Composite,
    Range,
    Multirange,
}

/// A parsed identity: the type's kind and OID, and the number of array dimensions around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserTypeIdentity {
    pub kind: UserTypeKind,
    pub oid: u32,
    pub dimensions: usize,
}

impl UserTypeIdentity {
    /// Parse `enum#<oid>`, `domain#<oid>` or `composite#<oid>` with any number of `[]` suffixes.
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
        } else if let Some(oid) = element.strip_prefix(COMPOSITE_PREFIX) {
            (UserTypeKind::Composite, oid)
        } else if let Some(oid) = element.strip_prefix(RANGE_PREFIX) {
            (UserTypeKind::Range, oid)
        } else if let Some(oid) = element.strip_prefix(MULTIRANGE_PREFIX) {
            (UserTypeKind::Multirange, oid)
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
    /// Whether a retained value of this type may name database-owned catalog identities. Anonymous records and polymorphic arrays may contain user-defined fields or elements, so their original database remains required even without a declared outer OID.
    #[must_use]
    pub fn is_catalog_dependent(&self) -> bool {
        let mut ty = self;
        while let Self::Array(element) = ty {
            ty = element;
        }
        matches!(
            ty,
            Self::Named(_)
                | Self::Enum(_)
                | Self::Composite(_)
                | Self::Domain { .. }
                | Self::DeclaredRange(_)
                | Self::DeclaredMultirange(_)
                | Self::Record
                | Self::AnyArray
        )
    }

    /// The identity of a user-defined type or an array of one; built-in types have none.
    #[must_use]
    pub fn user_type_identity(&self) -> Option<String> {
        self.user_type_identity_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary catalog identity cannot be cancelled or limited")
            .map(|value| {
                value
                    .into_uncontrolled()
                    .expect("ordinary catalog identity")
            })
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
        assert!(array.is_catalog_dependent());
        assert!(ColumnType::Record.is_catalog_dependent());
        assert!(!ColumnType::Array(Box::new(ColumnType::Integer)).is_catalog_dependent());
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
            UserTypeIdentity::parse("composite#9[]"),
            Some(UserTypeIdentity {
                kind: UserTypeKind::Composite,
                oid: 9,
                dimensions: 1,
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
