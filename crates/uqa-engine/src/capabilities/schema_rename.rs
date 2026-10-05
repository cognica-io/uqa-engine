//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind `ALTER SCHEMA ... RENAME TO` to the catalogs: the members of a schema are listed from the durable registries, each is moved by the lifecycle that renames it, and the schema's row moves to its new name with its identity.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::namespaces::{
    rename::{
        SchemaMemberRelocation, SchemaMembers, SchemaRelation, SchemaRelationKind,
        SchemaRenameContext, SchemaRenamePersistence, SchemaRenameRegistry,
    },
    SchemaRegistryWrite,
};
use uqa_execution::statement::context::schemas::{SchemaRenameTransactions, SchemaRenameWrite};
use uqa_sql::{
    ast::{
        AlterSequence, SequenceBound, SequenceLifecycle, SequenceOwnership, SequenceRestart,
        TypeObjectKind,
    },
    catalog::{security::BoundSchemaSecurity, view::StoredViewKind},
    SQLError, SQLResult,
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn relation_schema_context(
        &self,
    ) -> uqa_execution::schema::relation_alteration::relocation::RelationSchemaContext<'_> {
        uqa_execution::schema::relation_alteration::relocation::RelationSchemaContext {
            creation: self.relation_creation_context(),
            names: self,
            locks: self,
            indexes: self.index_registry_context(),
            sequences: self.sequence_lifecycle_context(),
        }
    }
    pub(crate) fn schema_rename_context(&self) -> SchemaRenameContext<'_> {
        SchemaRenameContext {
            tuples: self.schema_lock_context(),
            writer: self,
            refresh: self,
            session: self,
            roles: self,
            database: self,
            catalog: self,
            locks: self,
            schemas: self,
            registry: self,
            members: self,
            relocation: self,
            persistence: self,
            changes: self,
        }
    }

    /// Retain index identities and namespace destinations before their table moves.
    fn prepare_catalog_index_relocation(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<uqa_execution::schema::indexes::relocation::PreparedIndexRelocations, SQLError>
    {
        uqa_execution::schema::indexes::relocation::prepare_table_indexes(
            &self.index_registry_context(),
            &self.relation_creation_context(),
            self,
            from,
            to,
        )
    }
}

impl SchemaRenameTransactions for Engine {
    fn with_rename_write(&self, operation: SchemaRenameWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| operation(&engine.schema_rename_context()))
    }
}

impl SchemaRenameRegistry for Engine {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_> {
        Box::new(self.durable.schemas.write())
    }
    fn contains_graph(&self, name: &str) -> bool {
        self.durable.graphs.read().contains_key(name)
    }
    fn temporary_schema_name(&self) -> String {
        Engine::temporary_schema_name(self)
    }
}

impl SchemaMembers for Engine {
    fn relations(&self, schema: &str) -> StorageBackendResult<Vec<SchemaRelation>> {
        self.synchronize_catalog_registries()?;
        let mut relations = Vec::new();
        for relation in self.storage.tables.read().keys() {
            if relation.schema == schema {
                relations.push(SchemaRelation {
                    identity: relation.clone(),
                    kind: SchemaRelationKind::Table,
                });
            }
        }
        for (relation, view) in self.durable.views.read().iter() {
            if relation.schema == schema {
                relations.push(SchemaRelation {
                    identity: relation.clone(),
                    kind: match view.definition.kind {
                        StoredViewKind::View => SchemaRelationKind::View,
                        StoredViewKind::Materialized => SchemaRelationKind::MaterializedView,
                    },
                });
            }
        }
        for relation in self.durable.sequences.read().keys() {
            if relation.schema == schema {
                relations.push(SchemaRelation {
                    identity: relation.clone(),
                    kind: SchemaRelationKind::Sequence,
                });
            }
        }
        for relation in self.durable.foreign_tables.read().keys() {
            if relation.schema == schema {
                relations.push(SchemaRelation {
                    identity: relation.clone(),
                    kind: SchemaRelationKind::ForeignTable,
                });
            }
        }
        Ok(relations)
    }

    fn routines(&self, schema: &str) -> Vec<String> {
        self.durable
            .sql_user_functions
            .read()
            .keys()
            .filter(|key| {
                RelationIdentity::from_legacy_name(key)
                    .is_ok_and(|identity| identity.schema == schema)
            })
            .cloned()
            .collect()
    }

    fn types(&self, schema: &str) -> Vec<(String, TypeObjectKind)> {
        let in_schema = |key: &String| {
            RelationIdentity::from_legacy_name(key).is_ok_and(|identity| identity.schema == schema)
        };
        let mut types = Vec::new();
        for key in self
            .durable
            .enums
            .read()
            .keys()
            .filter(|key| in_schema(key))
        {
            types.push((key.clone(), TypeObjectKind::Type));
        }
        for key in self
            .durable
            .composites
            .read()
            .keys()
            .filter(|key| in_schema(key))
        {
            types.push((key.clone(), TypeObjectKind::Type));
        }
        for key in self
            .durable
            .domains
            .read()
            .keys()
            .filter(|key| in_schema(key))
        {
            types.push((key.clone(), TypeObjectKind::Domain));
        }
        types
    }
}

impl SchemaMemberRelocation for Engine {
    fn relocate_relation(&self, relation: &SchemaRelation, schema: &str) -> Result<(), SQLError> {
        let from = relation.identity.qualified_name();
        let to = RelationIdentity::new(schema, &relation.identity.name);
        match relation.kind {
            SchemaRelationKind::Table => {
                let indexes = self.prepare_catalog_index_relocation(&relation.identity, &to)?;
                let moved = self
                    .try_rename_table_inner(&from, &to.qualified_name())
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "move table `{from}` to schema `{schema}`: {error}"
                        ))
                    })?;
                if !moved {
                    return Err(SQLError::Internal(format!(
                        "table `{from}` disappeared while its schema was renamed"
                    )));
                }
                indexes.publish(self)
            }
            SchemaRelationKind::View => uqa_execution::schema::view_alteration::relocate_view(
                &self.view_alter_context(),
                &relation.identity,
                schema,
                "view",
            ),
            SchemaRelationKind::MaterializedView => {
                uqa_execution::schema::view_alteration::relocate_view(
                    &self.view_alter_context(),
                    &relation.identity,
                    schema,
                    "materialized view",
                )
            }
            SchemaRelationKind::Sequence => {
                let persistence =
                    uqa_execution::schema::sequences::dispatch::SequenceCommandCatalog::sequence_persistence(
                        self,
                        &relation.identity,
                    );
                let alter = AlterSequence {
                    name: from.clone(),
                    if_exists: false,
                    restart: SequenceRestart::default(),
                    increment: None,
                    start: None,
                    data_type: None,
                    min_value: SequenceBound::default(),
                    max_value: SequenceBound::default(),
                    cycle: None,
                    cache_size: None,
                    ownership: SequenceOwnership::default(),
                    persistence: None,
                    role_owner: None,
                    lifecycle: SequenceLifecycle::SetSchema {
                        schema: schema.to_string(),
                    },
                };
                uqa_execution::schema::sequences::lifecycle::alter_sequence_lifecycle(
                    &self.sequence_lifecycle_context(),
                    &from,
                    &relation.identity,
                    persistence,
                    &alter,
                )
            }
            SchemaRelationKind::ForeignTable => {
                uqa_execution::schema::foreign_table_alteration::relocate_foreign_table(
                    &self.foreign_table_alter_context(),
                    &relation.identity,
                    schema,
                )
            }
        }
    }

    fn relocate_routine(&self, registry_key: &str, schema: &str) -> Result<(), SQLError> {
        uqa_execution::routines::rename::relocate_sql_routines(
            &self.routine_rename_context(),
            registry_key,
            schema,
        )
    }

    fn relocate_type(
        &self,
        name: &str,
        kind: TypeObjectKind,
        schema: &str,
    ) -> Result<(), SQLError> {
        uqa_execution::schema::types::relocate_type_object(
            &self.type_lifecycle_context(),
            kind,
            name,
            schema,
        )
    }
}

impl SchemaRenamePersistence for Engine {
    fn save_schema_row(&self, name: &str, security: &BoundSchemaSecurity) -> Result<(), SQLError> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_schema_row(&security.row(name).into())
                .map_err(|error| {
                    SQLError::Internal(format!("store schema `{name}` for its rename: {error}"))
                })?;
        }
        Ok(())
    }

    fn drop_schema_row(&self, name: &str) -> Result<(), SQLError> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_schema(name).map_err(|error| {
                SQLError::Internal(format!(
                    "remove schema row `{name}` after its rename: {error}"
                ))
            })?;
        }
        Ok(())
    }
}
