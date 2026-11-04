//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    is_temporary_schema_name, CommentCaptureSource, CommentTableSource, RelationIdentity,
    RelationPersistence, SQLError, Scope, PROCEDURE_CLASS, TYPE_CLASS,
};

impl Scope<'_, '_> {
    pub(super) fn exclude_types(&self, source: &mut CommentCaptureSource) {
        if source.has_class(TYPE_CLASS) {
            let relation_types = self
                .tables
                .values()
                .filter(|table| table.persistence == RelationPersistence::Temporary)
                .map(CommentTableSource::oids)
                .chain(
                    self.definitions
                        .views
                        .values()
                        .filter(|view| view.persistence == RelationPersistence::Temporary)
                        .map(|view| view.relation_oids()),
                )
                .chain(
                    self.definitions
                        .foreign_tables
                        .values()
                        .filter(|table| table.persistence == RelationPersistence::Temporary)
                        .map(crate::catalog::foreign::StoredForeignTable::relation_oids),
                );
            for ids in relation_types {
                for oid in [ids.row_type, ids.array_type].into_iter().flatten() {
                    source.exclude(TYPE_CLASS, oid);
                }
            }
            for value in self
                .definitions
                .domains
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                source.exclude(TYPE_CLASS, value.oid);
                source.exclude(
                    TYPE_CLASS,
                    uqa_sql::catalog::type_metadata::pg_domain_array_oid(value.oid, value.array_oid)
                        as u32,
                );
            }
            for value in self
                .definitions
                .enums
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                source.exclude(TYPE_CLASS, value.oid);
                source.exclude(TYPE_CLASS, value.array_oid);
            }
            for value in self
                .definitions
                .composites
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                source.exclude(TYPE_CLASS, value.oid);
                source.exclude(TYPE_CLASS, value.array_oid);
            }
            for value in self
                .definitions
                .ranges
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                for oid in value.type_oids() {
                    source.exclude(TYPE_CLASS, oid);
                }
            }
            for value in self
                .definitions
                .shells
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                source.exclude(TYPE_CLASS, value.oid);
            }
        }
        let label_class =
            uqa_sql::schema::constraint_metadata::CatalogOidClass::EnumLabel.class_id();
        if source.has_class(label_class) {
            for value in self
                .definitions
                .enums
                .values()
                .filter(|value| is_temporary_schema_name(&value.identity.schema))
            {
                for oid in value.label_oids() {
                    source.exclude(label_class, oid);
                }
            }
        }
    }

    pub(super) fn exclude_routines(
        &self,
        source: &mut CommentCaptureSource,
    ) -> Result<(), SQLError> {
        if !source.has_class(PROCEDURE_CLASS) {
            return Ok(());
        }
        for (name, functions) in self.definitions.sql_user_functions.iter() {
            let name = RelationIdentity::borrowed_reference(name).ok_or_else(|| {
                SQLError::Internal("comment object has an invalid original routine name".into())
            })?;
            if !name
                .schema
                .is_some_and(uqa_sql::catalog::temporary_namespace::is_temporary_schema_reference)
            {
                continue;
            }
            for function in functions {
                source.exclude(
                    PROCEDURE_CLASS,
                    crate::catalog::projection::user_routine_catalog_oid(function)? as u32,
                );
            }
        }
        Ok(())
    }
}
