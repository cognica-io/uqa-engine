//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of relations as `heap_create_with_catalog`, `StoreCatalogInheritance1`, `StorePartitionKey`, `DefineView` and `process_owned_by` record them: the schema, the column types, the row type and its array type, the inheritance parents, the partition key, a view's `_RETURN` rule and a sequence's owning column.

use super::{ColumnScope, DependencyBuilder, References, RelationObject};
use uqa_core::RelationIdentity;
use uqa_sql::ast::ColumnDef;
use uqa_sql::catalog::dependencies::{
    DependencyKind, ObjectAddress, FOREIGN_SERVER_CLASS, RELATION_CLASS, REWRITE_CLASS, TYPE_CLASS,
};
use uqa_sql::catalog::relation_oids::RelationCatalogOids;
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_relations(&mut self) -> Result<(), SQLError> {
        let snapshot = self.catalog.snapshot();
        for (identity, table) in &snapshot.tables {
            let oid = table.catalog_oids.relation;
            self.record_relation(identity, oid, &table.columns, table.catalog_oids);
            self.record_hierarchy(oid, &table.hierarchy)?;
        }
        for (identity, view) in snapshot.definitions.views.iter() {
            let oids = view.relation_oids();
            let relation = self.relation_object(oids.relation)?;
            let columns = relation.columns.clone();
            self.record_relation(identity, oids.relation, &columns, oids);
            if let Some(rule) = oids.rule {
                self.record_view_rule(rule, oids.relation, &view.query)?;
            }
        }
        for (identity, table) in snapshot.definitions.foreign_tables.iter() {
            let oids = table.relation_oids();
            self.record_relation(identity, oids.relation, &table.columns, oids);
            let server = ObjectAddress::whole(FOREIGN_SERVER_CLASS, table.server_oid()?);
            // A concurrently removed server is still an unpinned reference; it must not disappear or bind a same-name replacement.
            self.objects.unpin(server);
            self.recorder.record(
                ObjectAddress::whole(RELATION_CLASS, oids.relation),
                server,
                DependencyKind::Normal,
            );
        }
        for (identity, object_id) in snapshot.definitions.sequence_object_ids.iter() {
            let oid = super::catalog_oid(
                crate::catalog::sequence::catalog_oids::sequence_catalog_oid(
                    &snapshot.definitions.sequence_catalog_oids,
                    object_id,
                ),
            )?;
            let sequence = ObjectAddress::whole(RELATION_CLASS, oid);
            self.record_namespace(sequence, &identity.schema);
            let Some(owner) = snapshot
                .definitions
                .sequences
                .get(identity)
                .and_then(|state| state.owner.as_ref())
            else {
                continue;
            };
            let Some(column) = self.owned_column(owner.table_object_id, owner.column_object_id)
            else {
                continue;
            };
            let kind = match owner.dependency {
                uqa_core::catalog_sequence::SequenceOwnerDependency::Automatic => {
                    DependencyKind::Auto
                }
                uqa_core::catalog_sequence::SequenceOwnerDependency::Internal => {
                    DependencyKind::Internal
                }
            };
            self.recorder.record(sequence, column, kind);
        }
        Ok(())
    }

    /// The schema, the column types, then the row type and its array type.
    fn record_relation(
        &mut self,
        identity: &RelationIdentity,
        oid: u32,
        columns: &[ColumnDef],
        oids: RelationCatalogOids,
    ) {
        let relation = ObjectAddress::whole(RELATION_CLASS, oid);
        self.record_namespace(relation, &identity.schema);
        for (index, column) in columns.iter().enumerate() {
            let Ok(number) =
                uqa_sql::catalog::relation_attributes::column_number(column, index).map(i32::from)
            else {
                continue;
            };
            let mut references = References::default();
            if let Ok(ty) = u32::try_from(uqa_sql::catalog::type_metadata::pg_type_oid(&column.ty))
            {
                references.add_type(ty);
            }
            self.recorder.record_references(
                ObjectAddress::column(oid, number),
                references,
                DependencyKind::Normal,
            );
        }
        if let Some(row_type) = oids.row_type {
            let row_type = ObjectAddress::whole(TYPE_CLASS, row_type);
            self.recorder
                .record(row_type, relation, DependencyKind::Internal);
            if let Some(array) = oids.array_type {
                self.recorder.record(
                    ObjectAddress::whole(TYPE_CLASS, array),
                    row_type,
                    DependencyKind::Internal,
                );
            }
        }
    }

    /// A child depends on each inheritance parent, a partition automatically on its partitioned parent; a partitioned table's key columns are part of it.
    fn record_hierarchy(
        &mut self,
        oid: u32,
        hierarchy: &uqa_sql::ast::TableHierarchy,
    ) -> Result<(), SQLError> {
        let relation = ObjectAddress::whole(RELATION_CLASS, oid);
        let kind = if hierarchy.partition_bound.is_some() {
            DependencyKind::Auto
        } else {
            DependencyKind::Normal
        };
        for parent in &hierarchy.parents {
            if let Some(parent) = self.objects.relation_oid_by_name(parent) {
                self.recorder
                    .record(relation, ObjectAddress::whole(RELATION_CLASS, parent), kind);
            }
        }
        let Some(spec) = &hierarchy.partition_spec else {
            return Ok(());
        };
        let table = self.relation_object(oid)?.clone();
        let mut expressions = References::default();
        let mut key_columns = Vec::new();
        for key in &spec.keys {
            match key {
                uqa_sql::ast::Expr::Column(name) => key_columns.extend(table.column_number(name)),
                expression => self.expressions().collect(
                    expression,
                    ColumnScope::Relation(oid, &table),
                    &mut expressions,
                )?,
            }
        }
        // `StorePartitionKey`: objects the key expressions use, with their own columns made part of the table, then the key columns.
        self.recorder.record_single_relation(
            relation,
            expressions,
            oid,
            (DependencyKind::Normal, DependencyKind::Internal),
            true,
        );
        for column in key_columns {
            self.recorder.record(
                ObjectAddress::column(oid, column),
                relation,
                DependencyKind::Internal,
            );
        }
        Ok(())
    }

    /// `InsertRule` for `ON SELECT`: the rule is part of the view and depends normally on what its query references.
    fn record_view_rule(
        &mut self,
        rule: u32,
        view: u32,
        query: &uqa_sql::plan::QueryPlan,
    ) -> Result<(), SQLError> {
        let address = ObjectAddress::whole(REWRITE_CLASS, rule);
        self.objects.add_member(
            REWRITE_CLASS,
            rule,
            super::MemberObject::Rule {
                name: "_RETURN".into(),
                relation: view,
            },
        );
        self.recorder.record(
            address,
            ObjectAddress::whole(RELATION_CLASS, view),
            DependencyKind::Internal,
        );
        let references = self.query_references(query)?;
        self.recorder
            .record_references(address, references, DependencyKind::Normal);
        Ok(())
    }

    /// The column of a table or foreign table a sequence is owned by, from the stable identities of the relation and column.
    fn owned_column(&self, table: [u8; 16], column: [u8; 16]) -> Option<ObjectAddress> {
        let snapshot = self.catalog.snapshot();
        let (oid, columns) = snapshot
            .tables
            .values()
            .find(|candidate| candidate.object_id == table)
            .map(|table| (table.catalog_oids.relation, table.columns.as_ref().clone()))
            .or_else(|| {
                snapshot
                    .definitions
                    .foreign_tables
                    .values()
                    .find(|candidate| candidate.object_id == table)
                    .map(|table| (table.relation_oids().relation, table.columns.clone()))
            })?;
        let index = columns
            .iter()
            .position(|candidate| candidate.object_id == Some(column))?;
        Some(ObjectAddress::column(
            oid,
            i32::from(
                uqa_sql::catalog::relation_attributes::column_number(&columns[index], index)
                    .ok()?,
            ),
        ))
    }

    pub(super) fn relation_object(&self, oid: u32) -> Result<&RelationObject, SQLError> {
        self.objects
            .relation(oid)
            .ok_or_else(|| SQLError::Internal(format!("relation {oid} is not in the catalog")))
    }
}
