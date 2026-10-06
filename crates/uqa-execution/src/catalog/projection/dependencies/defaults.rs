//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of column defaults and generation expressions as `StoreAttrDefault` records them: the `pg_attrdef` row goes with its column, as part of a generated one, and depends normally on what the expression coerced to the column type uses.

use super::{ColumnScope, DependencyBuilder, MemberObject, References};
use crate::catalog::projection::pg_catalog::attrdef_catalog_oid;
use uqa_sql::ast::ColumnDef;
use uqa_sql::catalog::dependencies::{DependencyKind, ObjectAddress, ATTRIBUTE_DEFAULT_CLASS};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_defaults(&mut self) -> Result<(), SQLError> {
        let snapshot = self.catalog.snapshot();
        let relations = snapshot
            .tables
            .iter()
            .map(|(identity, table)| {
                (
                    identity,
                    table.catalog_oids.relation,
                    table.columns.as_ref(),
                )
            })
            .chain(
                snapshot
                    .definitions
                    .foreign_tables
                    .iter()
                    .map(|(identity, table)| {
                        (identity, table.relation_oids().relation, &table.columns)
                    }),
            )
            .collect::<Vec<_>>();
        for (identity, oid, columns) in relations {
            for (index, column) in columns.iter().enumerate() {
                self.record_default(&identity.qualified_name(), oid, columns, index, column)?;
            }
        }
        Ok(())
    }

    fn record_default(
        &mut self,
        table_name: &str,
        relation: u32,
        columns: &[ColumnDef],
        index: usize,
        column: &ColumnDef,
    ) -> Result<(), SQLError> {
        let generated = column
            .generated
            .as_ref()
            .map(|generated| generated.expression.as_ref());
        let legacy_auto_increment = column
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy);
        let expression = generated.or(column.default.as_ref());
        if expression.is_none() && !legacy_auto_increment {
            return Ok(());
        }
        let number = i32::from(uqa_sql::catalog::relation_attributes::column_number(
            column, index,
        )?);
        let oid = super::catalog_oid(attrdef_catalog_oid(table_name, column))?;
        self.objects.add_member(
            ATTRIBUTE_DEFAULT_CLASS,
            oid,
            MemberObject::AttributeDefault {
                relation,
                column: number,
            },
        );
        let address = ObjectAddress::whole(ATTRIBUTE_DEFAULT_CLASS, oid);
        let kind = if generated.is_some() {
            DependencyKind::Internal
        } else {
            DependencyKind::Auto
        };
        self.recorder
            .record(address, ObjectAddress::column(relation, number), kind);
        let table = self.relation_object(relation)?.clone();
        let Some(expression) = expression else {
            // A counter column from a catalog written before sequence provenance defaults to `nextval` of the sequence named after it.
            let sequence = uqa_core::RelationIdentity::new(
                &table.identity.schema,
                format!("{}_{}_seq", table.identity.name, column.name),
            );
            if let Some(sequence) = self.objects.relation_oid(&sequence) {
                let mut references = References::default();
                references.add_relation(sequence);
                self.recorder
                    .record_references(address, references, DependencyKind::Normal);
            }
            return Ok(());
        };
        let mut references = References::default();
        self.expressions().collect_assigned(
            expression,
            &column.ty,
            (ColumnScope::Relation(relation, &table), columns),
            &mut references,
        )?;
        self.recorder.record_single_relation(
            address,
            references,
            relation,
            (DependencyKind::Normal, DependencyKind::Normal),
            false,
        );
        Ok(())
    }
}
