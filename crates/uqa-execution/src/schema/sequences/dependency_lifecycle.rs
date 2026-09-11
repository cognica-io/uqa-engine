//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect sequence dependents and publish ordered schema removals using native owners.
use crate::{
    catalog::{
        foreign::reads::ForeignTablesRead, sequence_introspection::SequenceIntrospectionCatalog,
    },
    schema::{
        constraints::ConstraintAlterContext,
        foreign_definitions::ForeignDefinitionContext,
        publication::{
            dependencies::{CatalogPublicationChanges, LoadedTableSchemas},
            removal::ColumnDropPublicationContext,
            SchemaPublicationContext,
        },
        view_dependencies::ViewDependencyContext,
    },
};
use std::{ops::Deref, sync::Arc};
use uqa_sql::{
    ast::{ColumnDef, TableCheck},
    catalog::events::definition::lookup::EventLookupContext,
    schema::sequences::{
        dependencies::analysis::{
            append_sequence_schema_expression_dependents, SequenceExpressionCatalog,
        },
        dependents::SequenceSchemaDependent,
    },
};
use uqa_storage::{SequenceOwner, StorageBackendError, StorageBackendResult};
pub type SequenceColumnsRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub type SequenceChecksRead<'a> = Box<dyn Deref<Target = Vec<TableCheck>> + 'a>;
pub trait SequenceTableMetadata {
    fn object_id(&self) -> [u8; 16];
    fn columns(&self) -> SequenceColumnsRead<'_>;
    fn table_checks(&self) -> SequenceChecksRead<'_>;
}
pub trait SequenceDependencyCatalog {
    fn refresh_tables(&self) -> StorageBackendResult<()>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn table_entries(&self) -> Vec<(String, Arc<dyn SequenceTableMetadata>)>;
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Arc<dyn SequenceTableMetadata>>>;
    fn foreign_tables(&self) -> ForeignTablesRead<'_>;
}
pub struct SequenceDependencyContext<'a> {
    pub catalog: &'a dyn SequenceDependencyCatalog,
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub expressions: &'a dyn SequenceExpressionCatalog,
    pub views: ViewDependencyContext<'a>,
    pub events: EventLookupContext<'a>,
    pub foreign: ForeignDefinitionContext<'a>,
    pub tables: &'a dyn LoadedTableSchemas,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub constraints: ConstraintAlterContext<'a>,
    pub schema: SchemaPublicationContext<'a>,
    pub columns: ColumnDropPublicationContext<'a>,
}
impl SequenceDependencyContext<'_> {
    pub fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>> {
        self.catalog.refresh_tables()?;
        self.catalog.refresh_catalog()?;
        self.sequences.refresh_sequences()?;
        let mut dependents = Vec::new();
        for (table_name, table) in self.catalog.table_entries() {
            append_sequence_schema_expression_dependents(
                self.expressions,
                &table_name,
                &table.columns(),
                &table.table_checks(),
                sequence,
                false,
                &mut dependents,
            )
            .map_err(StorageBackendError::Other)?;
        }
        for (relation, table) in self.catalog.foreign_tables().iter() {
            let table_name = relation.qualified_name();
            append_sequence_schema_expression_dependents(
                self.expressions,
                &table_name,
                &table.columns,
                &table.checks,
                sequence,
                true,
                &mut dependents,
            )
            .map_err(StorageBackendError::Other)?;
        }
        dependents.sort();
        dependents.dedup();
        Ok(dependents)
    }
    pub fn ensure_no_sequence_schema_dependencies(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        let dependents = self.sequence_schema_expression_dependents(sequence)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "column schema expression(s) `{}` depend on sequence `{sequence}`",
            dependents
                .iter()
                .map(SequenceSchemaDependent::object_label)
                .collect::<Vec<_>>()
                .join("`, `")
        )))
    }
    fn drop_sequence_schema_dependencies(
        &self,
        dependencies: &[SequenceSchemaDependent],
    ) -> StorageBackendResult<()> {
        for dependency in dependencies {
            let SequenceSchemaDependent::CheckConstraint {
                table,
                constraint,
                foreign,
            } = dependency
            else {
                continue;
            };
            if *foreign {
                if self
                    .foreign
                    .drop_foreign_table_check_dependency(table, constraint)?
                    != Some(true)
                {
                    return Err(StorageBackendError::Other(format!(
                        "constraint `{constraint}` on foreign table `{table}` disappeared after sequence DROP preflight"
                    )));
                }
            } else {
                crate::schema::constraints::drop::drop_constraint_dependency(
                    &self.constraints,
                    table,
                    constraint,
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::Default {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.foreign
                    .clear_foreign_table_default_dependency(table, column)?
                    == Some(true)
            } else {
                crate::schema::publication::columns::set_column_default(
                    &self.schema,
                    table,
                    column,
                    None,
                )?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "default `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::GeneratedColumn {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.foreign
                    .drop_foreign_table_column_dependency(table, column)?
                    == Some(true)
            } else {
                crate::schema::publication::removal::drop_column(
                    &self.columns,
                    table,
                    column,
                    false,
                )?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "generated column `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        Ok(())
    }
    pub fn detach_sequence_column_dependencies(
        &self,
        sequence: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let dependencies = self.sequence_schema_expression_dependents(sequence)?;
        if !cascade && !dependencies.is_empty() {
            self.ensure_no_sequence_schema_dependencies(sequence)?;
        }
        if cascade {
            self.drop_sequence_schema_dependencies(&dependencies)?;
        }
        let mut catalog_changed = false;
        for (_, table) in self.tables.table_schemas() {
            let mut columns = table.columns();
            let table_changed =
                uqa_sql::schema::sequences::dependencies::detach_sequence_provenance(
                    &mut columns,
                    sequence,
                );
            if !table_changed {
                continue;
            }
            table.persist_columns(&columns)?;
            table.write_columns().publish(columns);
            catalog_changed = true;
        }
        if catalog_changed {
            self.changes.table_catalog_changed();
        }
        self.foreign
            .detach_foreign_table_sequence_provenance(sequence)?;
        Ok(())
    }
    pub fn sequence_external_dependents_for_owner_drop(
        &self,
        sequence: &str,
        owner_drop_targets: &std::collections::BTreeSet<String>,
    ) -> StorageBackendResult<Vec<String>> {
        let mut dependents = self
            .sequence_schema_expression_dependents(sequence)?
            .into_iter()
            .filter(|dependency| !owner_drop_targets.contains(dependency.table()))
            .map(|dependency| dependency.object_label())
            .collect::<Vec<_>>();
        dependents.extend(
            crate::schema::view_dependencies::views_depending_on_sequence(&self.views, sequence)?
                .into_iter()
                .map(|view| format!("view {view}")),
        );
        dependents.extend(
            self.events
                .rules_depending_on_relations(&[sequence.to_string()])
                .map_err(uqa_storage::StorageBackendError::Other)?
                .into_iter()
                .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
        );
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }
    pub fn owned_sequence_dependents_for_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let canonical = self
            .catalog
            .resolve_table_name(table_name)?
            .ok_or_else(|| {
                StorageBackendError::Other(format!("table `{table_name}` does not exist"))
            })?;
        let table = self.catalog.table(&canonical)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{canonical}` disappeared"))
        })?;
        let column_object_id = table
            .columns()
            .iter()
            .find(|column| column.name == column_name)
            .and_then(|column| column.object_id)
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{canonical}`.`{column_name}` has no object identity"
                ))
            })?;
        let mut dependents = Vec::new();
        for sequence in
            crate::catalog::sequence_introspection::ownership::sequence_names_owned_by_column(
                self.sequences,
                table.object_id(),
                column_object_id,
            )?
        {
            dependents.extend(
                self.sequence_schema_expression_dependents(&sequence)?
                    .into_iter()
                    .filter(|dependency| !dependency.is_column(&canonical, column_name))
                    .map(|dependency| dependency.object_label()),
            );
            dependents.extend(
                crate::schema::view_dependencies::views_depending_on_sequence(
                    &self.views,
                    &sequence,
                )?
                .into_iter()
                .map(|view| format!("view {view}")),
            );
        }
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }
    pub fn sequence_owner_target(&self, owner: SequenceOwner) -> Option<(String, String, bool)> {
        self.catalog
            .table_entries()
            .into_iter()
            .find_map(|(table_name, table)| {
                if table.object_id() != owner.table_object_id {
                    return None;
                }
                table
                    .columns()
                    .iter()
                    .find(|column| column.object_id == Some(owner.column_object_id))
                    .map(|column| (table_name, column.name.clone(), false))
            })
            .or_else(|| {
                self.catalog
                    .foreign_tables()
                    .iter()
                    .find_map(|(relation, table)| {
                        if table.object_id != owner.table_object_id {
                            return None;
                        }
                        table
                            .columns
                            .iter()
                            .find(|column| column.object_id == Some(owner.column_object_id))
                            .map(|column| (relation.qualified_name(), column.name.clone(), true))
                    })
            })
    }
    pub fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        let mut refreshed = false;
        uqa_sql::schema::dependencies::rewrites::rewrite_sequence_function_references(
            expression,
            &mut |reference| {
                if !refreshed {
                    self.sequences
                        .refresh_sequences()
                        .map_err(|error| error.to_string())?;
                    refreshed = true;
                }
                *reference = self.expressions.stored_sequence_name(reference)?;
                Ok(())
            },
        )
        .map_err(StorageBackendError::Other)
    }
}
