//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The catalog objects that dependency addresses name, as the statements that remove them name them, and the addresses of objects a statement names.

use super::objects::{CatalogObjects, ConstraintOwner, MemberObject, RelationKind};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::dependencies::{
    ObjectAddress, ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, NAMESPACE_CLASS, PROCEDURE_CLASS,
    RELATION_CLASS, REWRITE_CLASS, TRIGGER_CLASS, TYPE_CLASS,
};

/// A user object of the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogObject {
    Relation {
        identity: RelationIdentity,
        kind: RelationKind,
        /// The table an index belongs to.
        table: Option<RelationIdentity>,
    },
    Column {
        relation: RelationIdentity,
        kind: RelationKind,
        name: String,
    },
    /// An enum or a domain.
    Type(u32),
    /// The array type of an enum, a domain or a relation's row type, which is part of its element type.
    ArrayType {
        element: u32,
    },
    /// A relation's row type, which is part of the relation.
    RowType {
        relation: u32,
    },
    RelationConstraint {
        relation: RelationIdentity,
        kind: RelationKind,
        name: String,
        /// A `NOT NULL` constraint, which is a property of its column.
        not_null: bool,
    },
    DomainConstraint {
        domain: u32,
        name: String,
        not_null: bool,
    },
    ColumnDefault {
        relation: RelationIdentity,
        kind: RelationKind,
        column: String,
    },
    Routine {
        object_id: [u8; 16],
    },
    Rule {
        relation: RelationIdentity,
        kind: RelationKind,
        name: String,
    },
    Trigger {
        relation: RelationIdentity,
        kind: RelationKind,
        name: String,
    },
    Schema(String),
}

/// A type the catalog defines, by the object it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeObject {
    /// An enum or a domain.
    Defined,
    Array {
        element: u32,
    },
    Row {
        relation: u32,
    },
}

impl CatalogObjects {
    /// The object `address` names; `None` for one that does not exist or is not a user object.
    pub(super) fn catalog_object(&self, address: ObjectAddress) -> Option<CatalogObject> {
        match address.class_id {
            RELATION_CLASS => {
                let relation = self.relation(address.object_id)?;
                if address.sub_id == 0 {
                    return Some(CatalogObject::Relation {
                        identity: relation.identity.clone(),
                        kind: relation.kind,
                        table: relation.table.clone(),
                    });
                }
                let index = usize::try_from(address.sub_id).ok()?.checked_sub(1)?;
                Some(CatalogObject::Column {
                    relation: relation.identity.clone(),
                    kind: relation.kind,
                    name: relation.columns.get(index)?.name.clone(),
                })
            }
            TYPE_CLASS => Some(match self.type_object(address.object_id)? {
                TypeObject::Defined => CatalogObject::Type(address.object_id),
                TypeObject::Array { element } => CatalogObject::ArrayType { element },
                TypeObject::Row { relation } => CatalogObject::RowType { relation },
            }),
            PROCEDURE_CLASS => self
                .routine_object_id(address.object_id)
                .map(|object_id| CatalogObject::Routine { object_id }),
            NAMESPACE_CLASS => self
                .namespace_name(address.object_id)
                .map(|name| CatalogObject::Schema(name.to_string())),
            CONSTRAINT_CLASS | ATTRIBUTE_DEFAULT_CLASS | REWRITE_CLASS | TRIGGER_CLASS => {
                self.member_object(address)
            }
            _ => None,
        }
    }

    fn member_object(&self, address: ObjectAddress) -> Option<CatalogObject> {
        let relation = |oid: &u32| {
            self.relation(*oid)
                .map(|relation| (relation.identity.clone(), relation.kind))
        };
        Some(match self.member(address.class_id, address.object_id)? {
            MemberObject::Constraint {
                name,
                owner: ConstraintOwner::Relation(owner),
                not_null,
            } => {
                let (relation, kind) = relation(owner)?;
                CatalogObject::RelationConstraint {
                    relation,
                    kind,
                    name: name.clone(),
                    not_null: *not_null,
                }
            }
            MemberObject::Constraint {
                name,
                owner: ConstraintOwner::Domain(domain),
                not_null,
            } => CatalogObject::DomainConstraint {
                domain: *domain,
                name: name.clone(),
                not_null: *not_null,
            },
            MemberObject::AttributeDefault {
                relation: owner,
                column,
            } => {
                let owner = self.relation(*owner)?;
                let index = usize::try_from(*column).ok()?.checked_sub(1)?;
                CatalogObject::ColumnDefault {
                    relation: owner.identity.clone(),
                    kind: owner.kind,
                    column: owner.columns.get(index)?.name.clone(),
                }
            }
            MemberObject::Rule {
                name,
                relation: owner,
            } => {
                let (relation, kind) = relation(owner)?;
                CatalogObject::Rule {
                    relation,
                    kind,
                    name: name.clone(),
                }
            }
            MemberObject::Trigger {
                name,
                relation: owner,
            } => {
                let (relation, kind) = relation(owner)?;
                CatalogObject::Trigger {
                    relation,
                    kind,
                    name: name.clone(),
                }
            }
            MemberObject::Membership { .. } => return None,
        })
    }

    /// The address of a relation, or of one of its columns.
    pub(super) fn relation_address(
        &self,
        identity: &RelationIdentity,
        column: Option<&str>,
    ) -> Option<ObjectAddress> {
        let oid = self.relation_oid(identity)?;
        match column {
            None => Some(ObjectAddress::whole(RELATION_CLASS, oid)),
            Some(name) => Some(ObjectAddress::column(
                oid,
                self.relation(oid)?.column_number(name)?,
            )),
        }
    }

    /// The address of the constraint, trigger or rule named `name` on the relation.
    pub(super) fn relation_member_address(
        &self,
        class_id: u32,
        identity: &RelationIdentity,
        name: &str,
    ) -> Option<ObjectAddress> {
        let relation = self.relation_oid(identity)?;
        self.members_of_class(class_id)
            .find(|(_, member)| match member {
                MemberObject::Constraint {
                    name: member,
                    owner: ConstraintOwner::Relation(owner),
                    ..
                }
                | MemberObject::Rule {
                    name: member,
                    relation: owner,
                }
                | MemberObject::Trigger {
                    name: member,
                    relation: owner,
                } => *owner == relation && member == name,
                _ => false,
            })
            .map(|(oid, _)| ObjectAddress::whole(class_id, oid))
    }

    /// The address of the default of a relation's column.
    pub(super) fn column_default_address(
        &self,
        identity: &RelationIdentity,
        column: &str,
    ) -> Option<ObjectAddress> {
        let relation = self.relation_oid(identity)?;
        let number = self.relation(relation)?.column_number(column)?;
        self.members_of_class(ATTRIBUTE_DEFAULT_CLASS)
            .find(|(_, member)| {
                matches!(member, MemberObject::AttributeDefault { relation: owner, column }
                    if *owner == relation && *column == number)
            })
            .map(|(oid, _)| ObjectAddress::whole(ATTRIBUTE_DEFAULT_CLASS, oid))
    }

    /// The address of a domain's constraint.
    pub(super) fn domain_constraint_address(
        &self,
        domain: u32,
        name: &str,
    ) -> Option<ObjectAddress> {
        self.members_of_class(CONSTRAINT_CLASS)
            .find(|(_, member)| {
                matches!(member, MemberObject::Constraint { name: member, owner: ConstraintOwner::Domain(owner), .. }
                    if *owner == domain && member == name)
            })
            .map(|(oid, _)| ObjectAddress::whole(CONSTRAINT_CLASS, oid))
    }
}
