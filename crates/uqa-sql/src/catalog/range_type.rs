//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared ranges retain four type identities and the selected support objects independently of their current names.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use super::roles::{identity::RoleSubject, RoleDefinition};
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

/// Check persisted structure and authority before exposing a generation. Declaration binding separately resolves the subtype, class, collation and support signatures against the complete catalog.
pub fn validate_range_registry(
    registry: &BTreeMap<String, StoredRange>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for (name, definition) in registry {
        for identity in [definition.object_id, definition.multirange.object_id] {
            if identity == [0; 16] || !identities.insert(identity) {
                return Err(format!("invalid or duplicate range identity for `{name}`"));
            }
        }
        for oid in definition.type_oids() {
            if oid < super::oids::FIRST_NORMAL_OBJECT_ID || !oids.insert(oid) {
                return Err(format!("invalid or duplicate range OID {oid} for `{name}`"));
            }
        }
        if definition.identity.qualified_name() != *name {
            return Err(format!("inconsistent range name for `{name}`"));
        }
        for (identity, array_name) in [
            (&definition.identity, &definition.array_name),
            (
                &definition.multirange.identity,
                &definition.multirange.array_name,
            ),
        ] {
            if identity.schema.is_empty()
                || !super::array_type_names::valid_type_name(&identity.name)
                || !super::array_type_names::valid_type_name(array_name)
                || !names.insert((&identity.schema, &identity.name))
                || !names.insert((&identity.schema, array_name))
            {
                return Err(format!("invalid or duplicate range type name for `{name}`"));
            }
        }
        if !definition.owner.is_valid() || definition.owner.role_definition(roles).is_none() {
            return Err(format!(
                "range `{name}` references missing role incarnation {}",
                definition.owner.oid
            ));
        }
        if !resolved_subtype(&definition.subtype)
            || definition.subtype_operator_class == 0
            || definition.canonical_oid == Some(0)
            || definition.subtype_difference_oid == Some(0)
        {
            return Err(format!("invalid range support identity for `{name}`"));
        }
    }
    Ok(())
}

fn resolved_subtype(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Named(_) => false,
        ColumnType::Array(element) | ColumnType::Domain { base: element, .. } => {
            resolved_subtype(element) && super::type_metadata::pg_type_oid(ty) != 0
        }
        _ => super::type_metadata::pg_type_oid(ty) != 0,
    }
}

#[cfg(test)]
mod tests;
