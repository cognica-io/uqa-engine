//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The user objects of one catalog snapshot with the OIDs their catalog rows hold, and the objects that dependencies may reference: what `IsPinnedObject` leaves unpinned.

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, ColumnType};
use uqa_sql::catalog::dependencies::{
    ObjectAddress, DATABASE_CLASS, LANGUAGE_CLASS, NAMESPACE_CLASS, PROCEDURE_CLASS,
    RELATION_CLASS, REWRITE_CLASS, ROLE_CLASS, TYPE_CLASS,
};
use uqa_sql::SQLError;

/// `FirstUnpinnedObjectId`: objects `initdb` creates below this OID are pinned.
const FIRST_UNPINNED_OBJECT_ID: u32 = 12_000;
/// `PG_PUBLIC_NAMESPACE`, which `initdb` creates unpinned.
pub(super) const PUBLIC_NAMESPACE: u32 = 2200;
/// The `plpgsql` language, which `initdb` installs unpinned.
pub(super) const PLPGSQL_LANGUAGE: u32 = 13_647;

/// A relation of the catalog and the kind `getRelationDescription` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    Index,
    Sequence,
    View,
    MaterializedView,
    ForeignTable,
    /// The relation of a standalone composite type, which belongs to the type.
    CompositeType,
}

impl RelationKind {
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Index => "index",
            Self::Sequence => "sequence",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::ForeignTable => "foreign table",
            Self::CompositeType => "composite type",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RelationObject {
    pub identity: RelationIdentity,
    pub kind: RelationKind,
    /// Attributes in column-number order.
    pub columns: Vec<ColumnDef>,
    /// The table an index belongs to.
    pub table: Option<RelationIdentity>,
}

impl RelationObject {
    pub(super) fn column_number(&self, name: &str) -> Option<i32> {
        self.columns
            .iter()
            .position(|column| column.name == name)
            .and_then(|index| i32::try_from(index + 1).ok())
    }
}

/// The relation or domain a constraint belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConstraintOwner {
    Relation(u32),
    Domain(u32),
}

/// An object that `getObjectDescription` names through the relation or roles it belongs to.
#[derive(Debug, Clone)]
pub(super) enum MemberObject {
    Constraint {
        name: String,
        owner: ConstraintOwner,
        /// A `NOT NULL` constraint, which is a property of its column.
        not_null: bool,
    },
    AttributeDefault {
        relation: u32,
        column: i32,
    },
    Rule {
        name: String,
        relation: u32,
    },
    Trigger {
        name: String,
        relation: u32,
    },
    Membership {
        member: u32,
        role: u32,
    },
}

#[derive(Debug, Clone, Default)]
pub(super) struct CatalogObjects {
    relations: BTreeMap<u32, RelationObject>,
    relation_oids: BTreeMap<RelationIdentity, u32>,
    routine_oids: BTreeMap<[u8; 16], u32>,
    /// The row type whose array type has the OID.
    row_type_arrays: BTreeMap<u32, u32>,
    types: BTreeMap<u32, super::addresses::TypeObject>,
    namespaces: BTreeMap<String, u32>,
    members: BTreeMap<(u32, u32), MemberObject>,
    roles: BTreeMap<u32, String>,
    unpinned: BTreeSet<(u32, u32)>,
}

impl CatalogObjects {
    /// The user objects of `catalog`, whose relations are named as `resolution` binds them.
    pub(super) fn collect(
        context: &CatalogContext<'_>,
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<Self, SQLError> {
        let mut objects = Self::default();
        objects.collect_namespaces(catalog);
        objects.collect_relations(context, catalog, resolution)?;
        objects.collect_types(catalog);
        for function in catalog.all_sql_functions() {
            let oid = catalog_oid(super::super::user_routine_catalog_oid(&function)?)?;
            if let Some(object_id) = function.def.object_id {
                objects.routine_oids.insert(object_id, oid);
            }
            objects.unpin(ObjectAddress::whole(PROCEDURE_CLASS, oid));
        }
        for role in catalog.roles() {
            let oid = catalog_oid(role.oid)?;
            objects.roles.insert(oid, role.name.clone());
            if oid >= FIRST_UNPINNED_OBJECT_ID {
                objects.unpin(ObjectAddress::whole(ROLE_CLASS, oid));
            }
        }
        objects.unpin(ObjectAddress::whole(
            DATABASE_CLASS,
            catalog_oid(uqa_sql::catalog::DATABASE_OID)?,
        ));
        objects.unpin(ObjectAddress::whole(LANGUAGE_CLASS, PLPGSQL_LANGUAGE));
        Ok(objects)
    }

    fn collect_namespaces(&mut self, catalog: &CatalogReadView) {
        for schema in catalog.all_schema_names() {
            let Ok(oid) =
                u32::try_from(super::super::helpers::oids::namespace_oid(catalog, &schema))
            else {
                continue;
            };
            self.namespaces.insert(schema.clone(), oid);
            // The system schemas are pinned except `public` and those `initdb` creates past the pinned range; extension schemas belong to the system catalog.
            let system = matches!(
                schema.as_str(),
                "pg_catalog" | uqa_sql::catalog::AG_CATALOG_SCHEMA
            );
            if !system && (oid >= FIRST_UNPINNED_OBJECT_ID || oid == PUBLIC_NAMESPACE) {
                self.unpin(ObjectAddress::whole(NAMESPACE_CLASS, oid));
            }
        }
    }

    /// The relation of each standalone composite type. Dropped attributes keep their numbers, so a column's position is its number.
    fn collect_composite_relations(&mut self, snapshot: &crate::catalog::CatalogReadSnapshot) {
        for definition in snapshot.definitions.composites.values() {
            let columns = definition
                .attributes
                .iter()
                .map(|attribute| {
                    let name = if attribute.dropped {
                        uqa_sql::catalog::composite_type::StoredCompositeAttribute::dropped_name(
                            attribute.number,
                        )
                    } else {
                        attribute.name.clone()
                    };
                    ColumnDef::nullable(name, attribute.ty.clone())
                })
                .collect();
            self.add_relation(
                definition.relation_oid,
                definition.identity.clone(),
                RelationKind::CompositeType,
                columns,
            );
        }
    }

    fn collect_relations(
        &mut self,
        context: &CatalogContext<'_>,
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<(), SQLError> {
        let snapshot = catalog.snapshot();
        for (identity, table) in &snapshot.tables {
            self.add_relation(
                table.catalog_oids.relation,
                identity.clone(),
                RelationKind::Table,
                table.columns.as_ref().clone(),
            );
            self.unpin_row_type(table.catalog_oids);
        }
        for (identity, view) in snapshot.definitions.views.iter() {
            let oids = view.relation_oids();
            let kind = match view.kind {
                uqa_sql::catalog::view::StoredViewKind::View => RelationKind::View,
                uqa_sql::catalog::view::StoredViewKind::Materialized => {
                    RelationKind::MaterializedView
                }
            };
            let columns =
                super::super::helpers::views::view_columns_for(context, catalog, resolution, view)?;
            self.add_relation(oids.relation, identity.clone(), kind, columns);
            self.unpin_row_type(oids);
            if let Some(rule) = oids.rule {
                self.unpin(ObjectAddress::whole(REWRITE_CLASS, rule));
            }
        }
        for (identity, table) in snapshot.definitions.foreign_tables.iter() {
            let oids = table.relation_oids();
            self.add_relation(
                oids.relation,
                identity.clone(),
                RelationKind::ForeignTable,
                table.columns.clone(),
            );
            self.unpin_row_type(oids);
        }
        self.collect_composite_relations(snapshot);
        for (identity, object_id) in snapshot.definitions.sequence_object_ids.iter() {
            let oid = catalog_oid(
                crate::catalog::sequence::catalog_oids::sequence_catalog_oid(
                    &snapshot.definitions.sequence_catalog_oids,
                    object_id,
                ),
            )?;
            // A sequence's row has the columns `pg_attribute` lists for it.
            let columns = [
                ("last_value", ColumnType::BigInteger),
                ("log_cnt", ColumnType::BigInteger),
                ("is_called", ColumnType::Boolean),
            ]
            .into_iter()
            .map(|(name, ty)| ColumnDef::nullable(name, ty))
            .collect();
            self.add_relation(oid, identity.clone(), RelationKind::Sequence, columns);
        }
        for index in super::super::pg_catalog::catalog_index_relations(catalog, resolution)? {
            let columns = index_attributes(&index);
            let oid = catalog_oid(index.oid())?;
            self.add_relation(oid, index.relation.clone(), RelationKind::Index, columns);
            if let Some(relation) = self.relations.get_mut(&oid) {
                relation.table = RelationIdentity::from_legacy_name(&index.table_name).ok();
            }
        }
        // Catalog views and `information_schema` relations that `initdb` creates past the pinned range.
        for relation in uqa_sql::catalog::SystemRelation::all() {
            let Ok(oid) = u32::try_from(relation.oid()) else {
                continue;
            };
            if oid < FIRST_UNPINNED_OBJECT_ID {
                continue;
            }
            let columns = relation
                .column_names()
                .into_iter()
                .map(|name| ColumnDef::nullable(name, ColumnType::Text))
                .collect();
            let kind = if relation.kind() == "view" {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            self.add_relation(
                oid,
                RelationIdentity::new(relation.namespace(), relation.name()),
                kind,
                columns,
            );
        }
        Ok(())
    }

    fn collect_types(&mut self, catalog: &CatalogReadView) {
        use super::addresses::TypeObject;
        for definition in catalog.enums() {
            self.add_type(definition.oid, TypeObject::Defined);
            self.add_type(
                definition.array_oid,
                TypeObject::Array {
                    element: definition.oid,
                },
            );
        }
        for definition in catalog.composites() {
            self.add_type(definition.oid, TypeObject::Defined);
            self.add_type(
                definition.array_oid,
                TypeObject::Array {
                    element: definition.oid,
                },
            );
        }
        for domain in catalog.domains() {
            self.add_type(domain.oid, TypeObject::Defined);
            if let Ok(array) = u32::try_from(uqa_sql::catalog::type_metadata::pg_domain_array_oid(
                domain.oid,
                domain.array_oid,
            )) {
                self.add_type(
                    array,
                    TypeObject::Array {
                        element: domain.oid,
                    },
                );
            }
        }
    }

    fn add_type(&mut self, oid: u32, object: super::addresses::TypeObject) {
        self.types.insert(oid, object);
        self.unpin(ObjectAddress::whole(TYPE_CLASS, oid));
    }

    fn add_relation(
        &mut self,
        oid: u32,
        identity: RelationIdentity,
        kind: RelationKind,
        columns: Vec<ColumnDef>,
    ) {
        self.relation_oids.insert(identity.clone(), oid);
        self.relations.insert(
            oid,
            RelationObject {
                identity,
                kind,
                columns,
                table: None,
            },
        );
        self.unpin(ObjectAddress::whole(RELATION_CLASS, oid));
    }

    fn unpin_row_type(&mut self, oids: uqa_sql::catalog::relation_oids::RelationCatalogOids) {
        use super::addresses::TypeObject;
        let Some(row_type) = oids.row_type else {
            return;
        };
        self.add_type(
            row_type,
            TypeObject::Row {
                relation: oids.relation,
            },
        );
        if let Some(array) = oids.array_type {
            self.add_type(array, TypeObject::Array { element: row_type });
            self.row_type_arrays.insert(array, row_type);
        }
    }

    /// Record an object that dependencies may reference.
    pub(super) fn unpin(&mut self, object: ObjectAddress) {
        self.unpinned.insert((object.class_id, object.object_id));
    }

    /// Name an object that `getObjectDescription` describes through the relation or roles it belongs to; it is unpinned.
    pub(super) fn add_member(&mut self, class_id: u32, oid: u32, member: MemberObject) {
        self.members.insert((class_id, oid), member);
        self.unpin(ObjectAddress::whole(class_id, oid));
    }

    pub(super) const fn unpinned(&self) -> &BTreeSet<(u32, u32)> {
        &self.unpinned
    }

    pub(super) fn relation(&self, oid: u32) -> Option<&RelationObject> {
        self.relations.get(&oid)
    }

    pub(super) fn relation_oid(&self, identity: &RelationIdentity) -> Option<u32> {
        self.relation_oids.get(identity).copied()
    }

    /// The OID of a relation named by its canonical or legacy qualified name.
    pub(super) fn relation_oid_by_name(&self, name: &str) -> Option<u32> {
        RelationIdentity::from_legacy_name(name)
            .ok()
            .and_then(|identity| self.relation_oid(&identity))
    }

    /// The row type of a relation whose array type has this OID.
    pub(super) fn row_type_of_array(&self, array: u32) -> Option<u32> {
        self.row_type_arrays.get(&array).copied()
    }

    pub(super) fn routine_oid(&self, object_id: &[u8; 16]) -> Option<u32> {
        self.routine_oids.get(object_id).copied()
    }

    pub(super) fn routine_object_id(&self, oid: u32) -> Option<[u8; 16]> {
        self.routine_oids
            .iter()
            .find_map(|(object_id, routine)| (*routine == oid).then_some(*object_id))
    }

    pub(super) fn type_object(&self, oid: u32) -> Option<super::addresses::TypeObject> {
        self.types.get(&oid).copied()
    }

    /// The members of one catalog, by OID.
    pub(super) fn members_of_class(
        &self,
        class_id: u32,
    ) -> impl Iterator<Item = (u32, &MemberObject)> + '_ {
        self.members
            .range((class_id, 0)..=(class_id, u32::MAX))
            .map(|((_, oid), member)| (*oid, member))
    }

    pub(super) fn namespace_oid(&self, schema: &str) -> Option<u32> {
        self.namespaces.get(schema).copied()
    }

    pub(super) fn namespace_name(&self, oid: u32) -> Option<&str> {
        self.namespaces
            .iter()
            .find_map(|(name, namespace)| (*namespace == oid).then_some(name.as_str()))
    }

    pub(super) fn role_name(&self, oid: u32) -> Option<&str> {
        self.roles.get(&oid).map(String::as_str)
    }

    pub(super) fn member(&self, class_id: u32, oid: u32) -> Option<&MemberObject> {
        self.members.get(&(class_id, oid))
    }

    /// Whether a dependency on `object` would be recorded.
    pub(super) fn is_unpinned(&self, object: ObjectAddress) -> bool {
        self.unpinned.contains(&(object.class_id, object.object_id))
    }
}

/// An index's attributes as `pg_attribute` names them: its key columns, then its included columns.
fn index_attributes(index: &super::super::pg_catalog::CatalogIndexRelation) -> Vec<ColumnDef> {
    let keys = index.columns.iter().enumerate().map(|(position, key)| {
        index
            .definition
            .key_names
            .get(position)
            .map(String::as_str)
            .or_else(|| key.column())
            .unwrap_or("expr")
            .to_string()
    });
    let included = index
        .definition
        .included_columns
        .iter()
        .enumerate()
        .map(|(position, name)| {
            index
                .definition
                .key_names
                .get(index.columns.len() + position)
                .unwrap_or(name)
                .clone()
        });
    keys.chain(included)
        .map(|name| ColumnDef::nullable(name, ColumnType::Text))
        .collect()
}

pub(super) fn catalog_oid(oid: i64) -> Result<u32, SQLError> {
    u32::try_from(oid).map_err(|_| SQLError::Internal(format!("catalog OID {oid} is out of range")))
}
