//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of indexes as `index_create` records them: an index that implements a constraint is part of it, any other goes automatically with the columns it indexes or with its table; a partition's index belongs to its parent's and to the partition; key expressions and the predicate depend normally on what they use and automatically on the table's columns.

use super::{ColumnScope, DependencyBuilder, References};
use crate::catalog::projection::pg_catalog::catalog_index_relations;
use uqa_core::RelationIdentity;
use uqa_sql::ast::IndexKey;
use uqa_sql::catalog::dependencies::{
    DependencyKind, ObjectAddress, CONSTRAINT_CLASS, RELATION_CLASS,
};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_indexes(&mut self) -> Result<(), SQLError> {
        let snapshot = self.catalog.snapshot();
        for index in catalog_index_relations(self.catalog, self.resolution)? {
            let oid = super::catalog_oid(index.oid())?;
            let address = ObjectAddress::whole(RELATION_CLASS, oid);
            let table_identity = RelationIdentity::from_legacy_name(&index.table_name)
                .map_err(SQLError::Internal)?;
            let Some(table_oid) = self.objects.relation_oid(&table_identity) else {
                continue;
            };
            let table = self.relation_object(table_oid)?.clone();
            let owning_constraint =
                index
                    .definition
                    .relationships
                    .owning_constraint
                    .and_then(|owner| {
                        snapshot.tables.get(&table_identity).and_then(|table| {
                            table
                                .keys
                                .iter()
                                .filter_map(|key| key.catalog_identity)
                                .find(|identity| identity.object_id == owner)
                        })
                    });
            if let Some(constraint) = owning_constraint {
                self.recorder.record(
                    address,
                    ObjectAddress::whole(CONSTRAINT_CLASS, super::catalog_oid(constraint.oid)?),
                    DependencyKind::Internal,
                );
            } else {
                let mut columns = References::default();
                let names = index
                    .columns
                    .iter()
                    .filter_map(IndexKey::column)
                    .chain(index.definition.included_columns.iter().map(String::as_str));
                for name in names {
                    if let Some(number) = table.column_number(name) {
                        columns.add_column(table_oid, number);
                    }
                }
                // Without simply referenced columns, the index goes with its table.
                if columns.is_empty() {
                    columns.add_relation(table_oid);
                }
                self.recorder
                    .record_references(address, columns, DependencyKind::Auto);
            }
            if let Some(parent) = index.parent_index_oid {
                self.recorder.record(
                    address,
                    ObjectAddress::whole(RELATION_CLASS, super::catalog_oid(parent)?),
                    DependencyKind::PartitionPrimary,
                );
                self.recorder.record(
                    address,
                    ObjectAddress::whole(RELATION_CLASS, table_oid),
                    DependencyKind::PartitionSecondary,
                );
            }
            let scope = ColumnScope::Relation(table_oid, &table);
            let mut expressions = References::default();
            for key in &index.columns {
                if let IndexKey::Expression(expression) = key {
                    self.expressions()
                        .collect(expression, scope, &mut expressions)?;
                }
            }
            if !expressions.is_empty() {
                self.recorder.record_single_relation(
                    address,
                    expressions,
                    table_oid,
                    (DependencyKind::Normal, DependencyKind::Auto),
                    false,
                );
            }
            if let Some(predicate) = &index.definition.predicate {
                let mut references = References::default();
                self.expressions()
                    .collect(predicate, scope, &mut references)?;
                self.recorder.record_single_relation(
                    address,
                    references,
                    table_oid,
                    (DependencyKind::Normal, DependencyKind::Auto),
                    false,
                );
            }
        }
        Ok(())
    }
}
