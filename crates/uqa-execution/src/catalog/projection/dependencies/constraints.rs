//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of relation constraints as `CreateConstraintEntry` records them: automatically on the constrained columns, a key constraint's INCLUDE columns among them, or the relation when none is named; normally on a foreign key's referenced columns and unique index, and on what a check expression uses; a partition's key constraint on its parent's, and a foreign key's constraint on a referenced partition internally on the foreign key.

use super::{ColumnScope, DependencyBuilder, MemberObject, References};
use crate::catalog::projection::helpers::constraints::{
    constraint_catalog_rows, ConstraintCatalogKind,
};
use crate::catalog::projection::pg_catalog::{
    catalog_index_relations, constraint_index_oid, constraint_parent_oid, constraint_row_oid,
};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::dependencies::{
    DependencyKind, ObjectAddress, CONSTRAINT_CLASS, RELATION_CLASS,
};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_constraints(&mut self) -> Result<(), SQLError> {
        let indexes = catalog_index_relations(self.catalog, self.resolution)?;
        for constraint in constraint_catalog_rows(self.catalog, self.resolution)? {
            let relation = RelationIdentity::new(&constraint.schema, &constraint.table);
            let Some(relation_oid) = self.objects.relation_oid(&relation) else {
                continue;
            };
            let oid = super::catalog_oid(constraint_row_oid(&constraint))?;
            self.objects.add_member(
                CONSTRAINT_CLASS,
                oid,
                MemberObject::Constraint {
                    name: constraint.name.clone(),
                    owner: super::ConstraintOwner::Relation(relation_oid),
                    not_null: matches!(constraint.kind, ConstraintCatalogKind::NotNull),
                },
            );
            let address = ObjectAddress::whole(CONSTRAINT_CLASS, oid);
            let mut constrained = References::default();
            if constraint.columns.is_empty() {
                constrained.add_relation(relation_oid);
            }
            for column in &constraint.columns {
                if let Ok(number) = i32::try_from(column.table_ordinal) {
                    constrained.add_column(relation_oid, number);
                }
            }
            // A key constraint covers every attribute of its index, its INCLUDE columns too, so dropping one of them drops the constraint.
            if matches!(
                constraint.kind,
                ConstraintCatalogKind::PrimaryKey | ConstraintCatalogKind::Unique { .. }
            ) {
                let table = self.relation_object(relation_oid)?.clone();
                for name in included_key_columns(self.catalog, &relation, constraint.object_id) {
                    if let Some(number) = table.column_number(&name) {
                        constrained.add_column(relation_oid, number);
                    }
                }
            }
            self.recorder
                .record_references(address, constrained, DependencyKind::Auto);
            if let Some(foreign_key) = &constraint.foreign_key {
                let mut referenced = References::default();
                let target = RelationIdentity::new(&foreign_key.schema, &foreign_key.table);
                if let Some(target) = self.objects.relation_oid(&target) {
                    for ordinal in &foreign_key.column_ordinals {
                        if let Ok(number) = i32::try_from(*ordinal) {
                            referenced.add_column(target, number);
                        }
                    }
                }
                if let Ok(index @ 1..) = u32::try_from(constraint_index_oid(&constraint, indexes)) {
                    referenced.add_relation(index);
                }
                self.recorder
                    .record_references(address, referenced, DependencyKind::Normal);
            }
            if let (ConstraintCatalogKind::Check, Some(expression)) =
                (constraint.kind, &constraint.expression)
            {
                let table = self.relation_object(relation_oid)?.clone();
                let mut references = References::default();
                self.expressions().collect(
                    expression,
                    ColumnScope::Relation(relation_oid, &table),
                    &mut references,
                )?;
                self.recorder.record_single_relation(
                    address,
                    references,
                    relation_oid,
                    (DependencyKind::Normal, DependencyKind::Normal),
                    false,
                );
            }
            if let Some(parent) = constraint.parent_oid {
                // `addFkRecurseReferenced`: a foreign key's constraint on a referenced partition is part of the foreign key, which goes with it and which deleting it deletes.
                if let Ok(parent @ 1..) = u32::try_from(parent) {
                    self.recorder.record(
                        address,
                        ObjectAddress::whole(CONSTRAINT_CLASS, parent),
                        DependencyKind::Internal,
                    );
                }
            } else if let Ok(parent @ 1..) =
                u32::try_from(constraint_parent_oid(self.catalog, &constraint, indexes))
            {
                // `index_constraint_create`: a partition's key constraint belongs to its parent's and to the partition.
                self.recorder.record(
                    address,
                    ObjectAddress::whole(CONSTRAINT_CLASS, parent),
                    DependencyKind::PartitionPrimary,
                );
                self.recorder.record(
                    address,
                    ObjectAddress::whole(RELATION_CLASS, relation_oid),
                    DependencyKind::PartitionSecondary,
                );
            }
        }
        Ok(())
    }
}

/// The INCLUDE columns of the key constraint `object_id` of `relation`.
fn included_key_columns(
    catalog: &crate::catalog::CatalogReadView,
    relation: &RelationIdentity,
    object_id: Option<[u8; 16]>,
) -> Vec<String> {
    let Some(object_id) = object_id else {
        return Vec::new();
    };
    catalog
        .snapshot()
        .tables
        .get(relation)
        .and_then(|table| {
            table.keys.iter().find(|key| {
                key.catalog_identity
                    .is_some_and(|identity| identity.object_id == object_id)
            })
        })
        .map(|key| key.included_columns.clone())
        .unwrap_or_default()
}
