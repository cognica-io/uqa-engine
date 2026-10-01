//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable standalone composite types: the type, its generated array type and the composite relation whose attributes describe it, as `DefineCompositeType` creates them.
//!
//! Attributes keep their `pg_attribute.attnum`. A dropped attribute stays in the definition with its number, as `PostgreSQL` keeps an `attisdropped` row, so later attributes keep their numbers and the number is never reused.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use super::roles::{identity::RoleSubject, RoleDefinition};
use crate::ast::{ColumnType, CompositeTypeReference, ObjectAclEntry};
use crate::expr::composites::{CompositeAttribute, CompositeTypeDescriptor};

const MAX_TYPE_NAME_BYTES: usize = 63;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCompositeAttribute {
    pub name: String,
    pub ty: ColumnType,
    /// The rendered qualified name of an explicit `COLLATE` clause; `None` uses the type's default collation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    /// `pg_attribute.attnum`.
    pub number: i16,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dropped: bool,
}

impl StoredCompositeAttribute {
    /// The name `pg_attribute` gives a dropped attribute.
    #[must_use]
    pub fn dropped_name(number: i16) -> String {
        format!("........pg.dropped.{number}........")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredComposite<Owner = RoleIdentity> {
    pub object_id: [u8; 16],
    /// The row type's `pg_type` OID.
    pub oid: u32,
    /// The composite relation's `pg_class` OID.
    pub relation_oid: u32,
    pub array_oid: u32,
    /// Name of the generated array type in the type's schema, which a later type can displace.
    pub array_name: String,
    pub identity: RelationIdentity,
    pub owner: Owner,
    /// Every attribute in attribute-number order, including dropped ones.
    pub attributes: Vec<StoredCompositeAttribute>,
    /// Explicit `USAGE` privileges; `None` is the default ACL (PUBLIC and the owner). The array type has no ACL of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_acl: Option<Vec<ObjectAclEntry>>,
}

impl<Owner> StoredComposite<Owner> {
    pub fn reference(&self) -> CompositeTypeReference {
        CompositeTypeReference {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            array_oid: self.array_oid,
            relation_oid: self.relation_oid,
        }
    }

    pub fn column_type(&self) -> ColumnType {
        ColumnType::Composite(self.reference())
    }

    /// The attributes that values carry, in attribute-number order.
    pub fn live_attributes(&self) -> impl Iterator<Item = &StoredCompositeAttribute> + '_ {
        self.attributes
            .iter()
            .filter(|attribute| !attribute.dropped)
    }

    pub fn descriptor(&self) -> CompositeTypeDescriptor {
        CompositeTypeDescriptor {
            type_oid: self.oid,
            relation_oid: self.relation_oid,
            attributes: self
                .live_attributes()
                .map(|attribute| CompositeAttribute {
                    name: attribute.name.clone(),
                    ty: attribute.ty.clone(),
                    number: attribute.number,
                })
                .collect(),
        }
    }

    /// The next attribute number: one past the highest number ever assigned, dropped attributes included.
    pub fn next_attribute_number(&self) -> i16 {
        self.attributes
            .iter()
            .map(|attribute| attribute.number)
            .max()
            .unwrap_or(0)
            + 1
    }
}

/// Check the definitions restored or published as one registry: identities and OIDs are unique, names agree with their keys, attribute numbers ascend and live names are distinct, and owners exist.
pub fn validate_composite_registry(
    registry: &BTreeMap<String, StoredComposite>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for (name, definition) in registry {
        if definition.object_id == [0; 16] || !identities.insert(definition.object_id) {
            return Err(format!(
                "invalid or duplicate composite identity for `{name}`"
            ));
        }
        for oid in [
            definition.oid,
            definition.array_oid,
            definition.relation_oid,
        ] {
            if oid < super::oids::FIRST_NORMAL_OBJECT_ID || !oids.insert(oid) {
                return Err(format!(
                    "invalid or duplicate composite OID {oid} for `{name}`"
                ));
            }
        }
        if definition.identity.schema.is_empty()
            || definition.identity.name.is_empty()
            || definition.identity.qualified_name() != *name
            || definition.array_name.is_empty()
            || definition.array_name.len() > MAX_TYPE_NAME_BYTES
        {
            return Err(format!("inconsistent composite name for `{name}`"));
        }
        let mut names = BTreeSet::new();
        if definition
            .attributes
            .windows(2)
            .any(|pair| pair[0].number >= pair[1].number)
            || definition
                .attributes
                .first()
                .is_some_and(|attribute| attribute.number < 1)
            || definition
                .live_attributes()
                .any(|attribute| !names.insert(attribute.name.as_str()))
        {
            return Err(format!("invalid composite attributes for `{name}`"));
        }
        if !definition.owner.is_valid() || definition.owner.role_definition(roles).is_none() {
            return Err(format!(
                "composite `{name}` references missing role incarnation {}",
                definition.owner.oid
            ));
        }
    }
    Ok(())
}
