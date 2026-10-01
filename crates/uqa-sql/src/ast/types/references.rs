//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog names carried by references to user-defined types. Identity is the OID; a rename or schema move gives every reference the type's current name.

use super::ColumnType;
use uqa_core::RelationIdentity;

impl ColumnType {
    /// Give every reference to the user-defined type `oid`, including array elements and domain bases, the type's current schema and name. Returns whether a reference changed.
    pub fn rename_user_type(&mut self, oid: u32, identity: &RelationIdentity) -> bool {
        match self {
            ColumnType::Enum(reference) if reference.oid == oid => {
                let changed =
                    reference.schema != identity.schema || reference.name != identity.name;
                reference.schema.clone_from(&identity.schema);
                reference.name.clone_from(&identity.name);
                changed
            }
            ColumnType::Composite(reference) if reference.oid == oid => {
                let changed =
                    reference.schema != identity.schema || reference.name != identity.name;
                reference.schema.clone_from(&identity.schema);
                reference.name.clone_from(&identity.name);
                changed
            }
            ColumnType::Domain {
                schema,
                name,
                oid: domain_oid,
                base,
                ..
            } => {
                let mut changed = base.rename_user_type(oid, identity);
                if *domain_oid == oid {
                    changed |= *schema != identity.schema || *name != identity.name;
                    schema.clone_from(&identity.schema);
                    name.clone_from(&identity.name);
                }
                changed
            }
            ColumnType::Array(element) => element.rename_user_type(oid, identity),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{ColumnType, EnumTypeReference};
    use uqa_core::RelationIdentity;

    #[test]
    fn renames_follow_type_identity_through_arrays_and_domain_bases() {
        let mood = ColumnType::Enum(EnumTypeReference {
            schema: "public".into(),
            name: "mood".into(),
            oid: 20_000,
            array_oid: 20_001,
        });
        let mut column = ColumnType::Array(Box::new(ColumnType::Domain {
            schema: "public".into(),
            name: "good_mood".into(),
            oid: 20_002,
            array_oid: None,
            base: Box::new(mood),
        }));
        let feeling = RelationIdentity::new("other", "feeling");
        assert!(column.rename_user_type(20_000, &feeling));
        assert!(!column.rename_user_type(20_000, &feeling));
        let ColumnType::Array(element) = &column else {
            panic!("array column");
        };
        let ColumnType::Domain { name, base, .. } = element.as_ref() else {
            panic!("domain element");
        };
        assert_eq!(name, "good_mood");
        assert!(
            matches!(base.as_ref(), ColumnType::Enum(reference) if reference.schema == "other" && reference.name == "feeling")
        );
        assert!(!column.rename_user_type(30_000, &feeling));
    }
}
