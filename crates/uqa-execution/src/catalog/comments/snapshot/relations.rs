//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    is_temporary_schema_name, CommentCaptureSource, RelationPersistence, SQLError, Scope,
    ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, RELATION_CLASS, REWRITE_CLASS,
};

impl Scope<'_, '_> {
    pub(super) fn exclude_relations(
        &self,
        source: &mut CommentCaptureSource,
    ) -> Result<(), SQLError> {
        if source.has_class(RELATION_CLASS) {
            self.exclude_relation_ids(source)?;
        }
        if source.has_class(ATTRIBUTE_DEFAULT_CLASS) {
            for (relation, columns) in self
                .tables
                .values()
                .filter(|table| table.persistence == RelationPersistence::Temporary)
                .map(|table| (table.relation, table.columns))
                .chain(
                    self.definitions
                        .foreign_tables
                        .iter()
                        .filter(|(_, table)| table.persistence == RelationPersistence::Temporary)
                        .map(|(name, table)| (name, table.columns.as_slice())),
                )
            {
                let name = relation.qualified_name();
                for column in columns.iter().filter(|column| {
                    column.default.is_some()
                        || column.generated.is_some()
                        || column
                            .auto_increment
                            .as_ref()
                            .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
                }) {
                    source.exclude(
                        ATTRIBUTE_DEFAULT_CLASS,
                        crate::catalog::projection::attrdef_catalog_oid(&name, column) as u32,
                    );
                }
            }
        }
        if source.has_class(CONSTRAINT_CLASS) {
            use uqa_sql::schema::constraint_metadata::identity::claims::constraint_oids;
            for table in self
                .tables
                .values()
                .filter(|table| table.persistence == RelationPersistence::Temporary)
            {
                for oid in constraint_oids(
                    table.relation,
                    table.columns,
                    table.checks,
                    table.keys,
                    table.foreign_keys,
                ) {
                    source.exclude(CONSTRAINT_CLASS, oid as u32);
                }
            }
            for (relation, table) in self
                .definitions
                .foreign_tables
                .iter()
                .filter(|(_, table)| table.persistence == RelationPersistence::Temporary)
            {
                for oid in constraint_oids(relation, &table.columns, &table.checks, &[], &[]) {
                    source.exclude(CONSTRAINT_CLASS, oid as u32);
                }
            }
            for domain in self
                .definitions
                .domains
                .values()
                .filter(|domain| is_temporary_schema_name(&domain.identity.schema))
            {
                for identity in
                    uqa_sql::schema::domains::constraints::identities(&domain.definition)
                {
                    source.exclude(CONSTRAINT_CLASS, identity.oid as u32);
                }
            }
        }
        if source.has_class(REWRITE_CLASS) {
            for view in self
                .definitions
                .views
                .values()
                .filter(|view| view.persistence == RelationPersistence::Temporary)
            {
                if let Some(oid) = view.relation_oids().rule {
                    source.exclude(REWRITE_CLASS, oid);
                }
            }
            for (_, rules) in self
                .definitions
                .rules
                .iter()
                .filter(|(relation, _)| self.temporary_relation_oid(relation).is_some())
            {
                for rule in rules.values() {
                    source.exclude(
                        REWRITE_CLASS,
                        crate::catalog::projection::rule_catalog_oid(rule) as u32,
                    );
                }
            }
        }
        Ok(())
    }

    fn exclude_relation_ids(&self, source: &mut CommentCaptureSource) -> Result<(), SQLError> {
        for table in self
            .tables
            .values()
            .filter(|table| table.persistence == RelationPersistence::Temporary)
        {
            source.exclude(RELATION_CLASS, table.oids().relation);
        }
        for view in self
            .definitions
            .views
            .values()
            .filter(|view| view.persistence == RelationPersistence::Temporary)
        {
            source.exclude(RELATION_CLASS, view.relation_oids().relation);
        }
        for table in self
            .definitions
            .foreign_tables
            .values()
            .filter(|table| table.persistence == RelationPersistence::Temporary)
        {
            source.exclude(RELATION_CLASS, table.relation_oids().relation);
        }
        for definition in self
            .definitions
            .composites
            .values()
            .filter(|value| is_temporary_schema_name(&value.identity.schema))
        {
            source.exclude(RELATION_CLASS, definition.relation_oid);
        }
        for (relation, _) in self
            .definitions
            .sequence_persistence
            .iter()
            .filter(|(_, persistence)| **persistence == RelationPersistence::Temporary)
        {
            let identity = self
                .definitions
                .sequence_object_ids
                .get(relation)
                .ok_or_else(|| {
                    SQLError::Internal(
                        "temporary sequence comment owner has no original identity".into(),
                    )
                })?;
            source.exclude(
                RELATION_CLASS,
                crate::catalog::sequence::catalog_oids::sequence_catalog_oid(
                    &self.definitions.sequence_catalog_oids,
                    identity,
                ) as u32,
            );
        }
        for index in self.definitions.catalog_indexes.values() {
            if !self
                .table_by_name(&index.table_name)?
                .is_some_and(|table| table.persistence == RelationPersistence::Temporary)
            {
                continue;
            }
            let identity = uqa_sql::catalog::index::stored::catalog_object_identity(
                index.definition_json.as_deref(),
            )
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| {
                SQLError::Internal("temporary index comment owner has no original identity".into())
            })?;
            source.exclude(RELATION_CLASS, identity.oid as u32);
        }
        Ok(())
    }
}
