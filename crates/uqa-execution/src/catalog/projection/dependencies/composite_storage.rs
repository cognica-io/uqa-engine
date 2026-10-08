//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expose catalog relation storage and row-type identities to SQL's recursive type-use checks.

use super::{CatalogDependencies, RelationKind};
use uqa_sql::schema::composites::type_changes::{reject_stored_uses, CompositeStorageUse};
use uqa_sql::SQLError;

impl CatalogDependencies {
    pub fn reject_stored_composite_uses(&self, oid: u32, name: &str) -> Result<(), SQLError> {
        reject_stored_uses(&self.graph, oid, name, |address, target| {
            let relation = self.objects.relation(address.object_id)?;
            let column = if address.sub_id > 0 {
                relation
                    .columns
                    .iter()
                    .find(|column| relation.column_number(&column.name) == Some(address.sub_id))
            } else {
                relation.columns.iter().find(|column| {
                    uqa_sql::catalog::type_metadata::pg_type_oid(&column.ty) == i64::from(target)
                })
            }?;
            if matches!(
                relation.kind,
                RelationKind::Table
                    | RelationKind::Index
                    | RelationKind::MaterializedView
                    | RelationKind::Sequence
            ) {
                Some(CompositeStorageUse::Stored {
                    relation: &relation.identity.name,
                    column: &column.name,
                })
            } else {
                relation.row_type.map(CompositeStorageUse::RowType)
            }
        })
    }
}
