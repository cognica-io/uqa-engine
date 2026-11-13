//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained table generations and field-root publication adapt the shared installation owner.

use crate::{
    state::{CatalogGeneration, TableRegistryMap},
    Engine, TableState,
};
use std::sync::Arc;
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources, CompiledAnalyzer};
use uqa_core::{image_io::AsyncImageInput, memory::RetainedValue, RelationIdentity};
use uqa_execution::catalog::analyzers::fields::{
    installation::{
        self, AnalyzerCatalogState, AnalyzerTable, AnalyzerTableDefinition, AnalyzerTables,
        BindingMigrations, FieldAnalyzerInstallation,
    },
    CatalogFieldBindings, FieldAnalyzerCatalogInput, GinAnalyzerOwners, RestoredFieldAnalyzers,
};
use uqa_execution::catalog::analyzers::gin::{
    self, GinAnalyzerCatalogInput, GinAnalyzerInputTable, GinAnalyzerInputTables, GinAnalyzerTable,
    GinAnalyzerTables, RetainedGinAnalyzerOwners,
};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence},
    catalog::{table_definition::SharedTableDefinition, table_metadata::SharedTableMetadata},
};
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog, catalog::input::CatalogInputLimits,
    read_control::StorageReadControl, CatalogFacade, FieldAnalyzerBinding, StorageBackendError,
    StorageBackendResult,
};

#[derive(Clone)]
struct Table(Arc<TableState>);

impl AnalyzerTableDefinition for Table {
    fn fields(&self) -> SharedTableMetadata<Vec<String>> {
        self.0.fts_fields.snapshot()
    }
    fn columns_declared(&self) -> bool {
        *self.0.columns_declared.read()
    }
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>> {
        self.0.columns.snapshot()
    }
}

impl AnalyzerTable for Table {
    fn persistence(&self) -> RelationPersistence {
        self.0.persistence
    }
    fn default_binding(&self, field: &str) -> StorageBackendResult<FieldAnalyzerBinding> {
        let index = self.0.inverted_index.read();
        Ok(FieldAnalyzerBinding::unassigned(
            index.index_analyzer_revision(field)?,
            index.search_analyzer_revision(field)?,
        ))
    }
    fn set_default(&self, definition: AnalyzerDefinition) {
        *self.0.analyzer.write() = definition;
    }
    fn install(&self, field: &str, binding: &FieldAnalyzerBinding) -> StorageBackendResult<()> {
        binding.install(field, self.0.inverted_index.write().as_mut())
    }
    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        self.0.inverted_index.read().source_rebuild_required()
    }
}

impl GinAnalyzerTable for Table {
    fn register_field(&self, field: &str) -> StorageBackendResult<()> {
        self.0.fts_fields.write().push(field.to_owned());
        Ok(())
    }
}

impl GinAnalyzerInputTable for Table {
    async fn register_field_from(
        &self,
        field: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        let fields = self
            .fields()
            .with_appended_name_from(field, input)
            .await
            .map_err(|error| StorageBackendError::backend("GIN field metadata", error))?;
        let generation = CatalogGeneration::with_budget(fields, input.memory)?;
        input.checkpoint().await?;
        self.0.fts_fields.restore(&generation);
        Ok(())
    }
}

struct GinTables<'a>(&'a Engine);

impl GinAnalyzerTables for GinTables<'_> {
    type Table = Table;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Table>> {
        self.0.try_table(name).map(|table| table.map(Table))
    }
}

struct Tables(RetainedValue<TableRegistryMap>);

impl AnalyzerTables for Tables {
    type Table = Table;
    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Table)> + Send {
        self.0
            .iter()
            .map(|(name, table)| (name, Table(Arc::clone(table))))
    }
}

impl GinAnalyzerInputTables for Tables {
    type Table = Table;
    async fn table_from(
        &self,
        name: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<Table>> {
        Ok(gin::lookup_table_from(&self.0, name, input)
            .await?
            .map(|table| Table(Arc::clone(table))))
    }
}

struct Catalog<'a>(&'a Engine);

impl AnalyzerCatalogState for Catalog<'_> {
    fn named(&self, name: &str) -> Option<Arc<CompiledAnalyzer>> {
        self.0.durable.named_analyzers.read().get(name).cloned()
    }
    fn fields(&self) -> FieldAnalyzerCatalog {
        self.0.durable.table_field_analyzers.read().clone()
    }
    fn publish(&mut self, fields: FieldAnalyzerCatalog) {
        *self.0.durable.table_field_analyzers.write() = fields;
    }
}

impl Engine {
    pub(crate) fn validate_table_analyzer_field(
        table_name: &str,
        table: &Arc<TableState>,
        field: &str,
    ) -> Result<(), String> {
        installation::validate_declared_field(&Table(Arc::clone(table)), table_name, field)
            .map_err(|error| error.to_string())
    }

    pub(super) fn restore_gin_analyzer_owners(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<GinAnalyzerOwners> {
        gin::restore(catalog, &GinTables(self))
    }

    pub(super) fn install_catalog_field_analyzers(
        &self,
        saved: CatalogFieldBindings,
        gin: &GinAnalyzerOwners,
        migrate: bool,
    ) -> StorageBackendResult<BindingMigrations> {
        installation::restore(
            Tables(self.storage.tables.live_snapshot()),
            Catalog(self),
            saved,
            gin,
            migrate,
        )
    }

    pub(crate) fn stored_gin_analyzers_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<GinAnalyzerCatalogInput> {
        let catalog = self.storage.catalog.as_ref().expect("persistent catalog");
        GinAnalyzerCatalogInput::new(
            catalog.as_ref(),
            Tables(self.storage.tables.live_snapshot()),
            limits,
            control,
        )
    }

    pub(crate) fn stored_field_analyzers_input(
        &self,
        resources: Arc<AnalyzerResources>,
        gin: RetainedGinAnalyzerOwners,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<FieldAnalyzerCatalogInput> {
        let catalog = self.storage.catalog.as_ref().expect("persistent catalog");
        FieldAnalyzerCatalogInput::new(catalog.as_ref(), resources, limits, gin, control)
    }

    pub(crate) fn restored_field_analyzers_installation(
        &self,
        saved: RestoredFieldAnalyzers,
        control: &StorageReadControl,
    ) -> StorageBackendResult<FieldAnalyzerInstallation> {
        FieldAnalyzerInstallation::new(
            Tables(self.storage.tables.live_snapshot()),
            saved,
            self.durable.named_analyzers.read().clone(),
            self.durable.table_field_analyzers.read().budget(),
            control,
        )
    }
}

#[cfg(test)]
mod tests;
