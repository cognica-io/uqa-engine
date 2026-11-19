//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared ranges retain four type identities and the selected support objects independently of their current names.

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use crate::ast::{ColumnType, ObjectAclEntry, RangeAlignment, RangeTypeReference};
use crate::expr::RangeValueDescriptor;

/// The associated multirange can be renamed or moved independently. Its owner and usage privileges follow the range; constructor routines have separate catalog records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMultirange {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub array_oid: u32,
    pub array_name: String,
    pub identity: RelationIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredRange {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub array_oid: u32,
    pub array_name: String,
    pub identity: RelationIdentity,
    pub owner: RoleIdentity,
    pub subtype: ColumnType,
    /// The selected class, not a class inferred again from the subtype after restoration.
    pub subtype_operator_class: u32,
    /// Zero for a noncollatable subtype, otherwise the selected collation OID.
    pub collation_oid: u32,
    /// Optional support routine identities, independent of routine names and the endpoint's native range family.
    pub canonical_oid: Option<u32>,
    pub subtype_difference_oid: Option<u32>,
    pub multirange: StoredMultirange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_acl: Option<Vec<ObjectAclEntry>>,
}

impl StoredRange {
    #[must_use]
    pub fn reference(&self) -> RangeTypeReference {
        RangeTypeReference {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            array_oid: self.array_oid,
            alignment: RangeAlignment::for_endpoint(&self.subtype),
        }
    }

    #[must_use]
    pub fn multirange_reference(&self) -> RangeTypeReference {
        RangeTypeReference {
            schema: self.multirange.identity.schema.clone(),
            name: self.multirange.identity.name.clone(),
            oid: self.multirange.oid,
            array_oid: self.multirange.array_oid,
            alignment: RangeAlignment::for_endpoint(&self.subtype),
        }
    }

    #[must_use]
    pub fn column_type(&self) -> ColumnType {
        ColumnType::DeclaredRange(self.reference())
    }

    #[must_use]
    pub fn multirange_type(&self) -> ColumnType {
        ColumnType::DeclaredMultirange(self.multirange_reference())
    }

    /// Derive read-only layout metadata once per catalog generation, without executing any selected support routine.
    #[must_use]
    pub fn descriptor(&self) -> RangeValueDescriptor {
        RangeValueDescriptor {
            range_oid: self.oid,
            multirange_oid: self.multirange.oid,
            endpoint: self.subtype.clone(),
        }
    }

    #[must_use]
    pub const fn type_oids(&self) -> [u32; 4] {
        [
            self.oid,
            self.array_oid,
            self.multirange.oid,
            self.multirange.array_oid,
        ]
    }

    #[must_use]
    pub fn has_type_name(&self, identity: &RelationIdentity) -> bool {
        [&self.identity, &self.multirange.identity].contains(&identity)
            || (self.identity.schema == identity.schema && self.array_name == identity.name)
            || (self.multirange.identity.schema == identity.schema
                && self.multirange.array_name == identity.name)
    }

    #[must_use]
    pub fn named_type(&self, schema: &str, name: &str) -> Option<ColumnType> {
        for (identity, array, multi) in [
            (&self.identity, &self.array_name, false),
            (&self.multirange.identity, &self.multirange.array_name, true),
        ] {
            if identity.schema != schema || (identity.name != name && array != name) {
                continue;
            }
            let ty = if multi {
                self.multirange_type()
            } else {
                self.column_type()
            };
            return Some(if array == name {
                ColumnType::Array(Box::new(ty))
            } else {
                ty
            });
        }
        None
    }
}

mod validation;
pub(super) use validation::resolved_subtype;
pub use validation::{
    validate_range_registry, validate_range_registry_from_input,
    validate_range_registry_with_control,
};

#[cfg(test)]
mod tests;
