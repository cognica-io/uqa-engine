//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore exact analyzer revisions and migrate legacy names within the owning catalog transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::{AnalyzerResources, CompiledAnalyzer};
use uqa_execution::catalog::analyzers::fields::GinAnalyzerOwners;
use uqa_sql::ast::{IndexKey, RelationPersistence};
use uqa_storage::{
    CatalogFacade, FieldAnalyzerBinding, RelationIdentity, StorageBackendError,
    StorageBackendResult,
};

use crate::open::CatalogRestoreMode;
use crate::{Engine, TableState};

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

fn canonical_table(name: &str) -> StorageBackendResult<String> {
    RelationIdentity::from_legacy_name(name)
        .map(|relation| relation.qualified_name())
        .map_err(corrupt)
}

impl Engine {
    pub(crate) fn current_field_analyzer_binding(
        &self,
        table_name: &str,
        table: &TableState,
        field: &str,
    ) -> StorageBackendResult<FieldAnalyzerBinding> {
        if let Some(binding) = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table_name.to_owned(), field.to_owned()))
        {
            return Ok(binding.clone());
        }
        let index = table.inverted_index.read();
        Ok(FieldAnalyzerBinding::unassigned(
            index.index_analyzer_revision(field)?,
            index.search_analyzer_revision(field)?,
        ))
    }

    pub(crate) fn persist_field_analyzer_binding(
        &self,
        table: &str,
        field: &str,
        binding: &FieldAnalyzerBinding,
    ) -> StorageBackendResult<()> {
        let json = binding.to_json()?;
        if self
            .try_table(table)?
            .is_some_and(|table| table.persistence == RelationPersistence::Temporary)
        {
            return Ok(());
        }
        if let Some(catalog) = &self.storage.catalog {
            let name = binding
                .last_assignment()
                .map_or_else(String::new, |(name, _)| name);
            catalog.replace_table_field_analyzer_binding(
                table,
                field,
                binding.phase_name(),
                &name,
                &json,
            )?;
        }
        Ok(())
    }

    pub(crate) fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let resources = AnalyzerResources::default();
        let names = uqa_execution::catalog::analyzers::named::restore(
            catalog,
            &resources,
            mode.allows_migration(),
            crate::parse_analyzer_config,
        )?;
        self.durable.named_analyzers.write().extend(names.current)?;
        self.restore_field_analyzers_from_catalog(catalog, &resources, mode, names.pending)
    }

    pub(crate) fn restore_field_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        resources: &AnalyzerResources,
        mode: CatalogRestoreMode,
        pending_names: Vec<(String, Arc<CompiledAnalyzer>)>,
    ) -> StorageBackendResult<()> {
        let gin_owners = self.restore_gin_analyzer_owners(catalog)?;
        let saved =
            uqa_execution::catalog::analyzers::fields::restore(catalog, resources, &gin_owners)?;
        let migrations =
            self.install_catalog_field_analyzers(saved, &gin_owners, mode.allows_migration())?;
        // Every descriptor and owner is validated before rebuilding sources or publishing analyzer metadata.
        for table_name in migrations.rebuild_tables {
            let table = self
                .try_table(&table_name)?
                .ok_or_else(|| corrupt("missing analyzer migration table"))?;
            Self::rebuild_fts_index(&table)?;
            self.try_save_table_schema(&table_name, &table)?;
        }
        for (name, compiled) in pending_names {
            let configuration = serde_json::to_string(&compiled.descriptor().configuration()?)?;
            catalog.save_analyzer_revision(
                &name,
                &configuration,
                compiled.descriptor().canonical_json(),
            )?;
        }
        for ((table, field), binding) in migrations.bindings {
            self.persist_field_analyzer_binding(&table, &field, &binding)?;
        }
        Ok(())
    }

    pub(crate) fn initialize_table_analyzer_bindings(
        &self,
        name: &str,
        table: &TableState,
        default_revision: Option<Arc<CompiledAnalyzer>>,
    ) -> StorageBackendResult<crate::TableFieldAnalyzerRegistry> {
        let mut bindings = self.durable.table_field_analyzers.read().clone();
        if let Some(revision) = default_revision {
            for field in table.fts_fields() {
                let binding = FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone());
                bindings.insert(
                    &(name.to_owned(), field.clone()),
                    binding.clone(),
                    table.persistence == RelationPersistence::Temporary,
                    &self.runtime.cancellation,
                )?;
                binding.install(&field, table.inverted_index.write().as_mut())?;
                if table.persistence != RelationPersistence::Temporary {
                    if let Some(catalog) = &self.storage.catalog {
                        catalog.replace_table_field_analyzer_binding(
                            name,
                            &field,
                            "both",
                            "",
                            &binding.to_json()?,
                        )?;
                    }
                }
            }
        }
        Ok(bindings)
    }

    pub(super) fn restore_gin_analyzer_owners(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<GinAnalyzerOwners> {
        let mut owners = BTreeMap::new();
        for row in catalog.load_catalog_indexes()? {
            if !row.index_type.eq_ignore_ascii_case("gin") {
                continue;
            }
            let table_name = canonical_table(&row.table_name)?;
            let table = self
                .try_table(&table_name)?
                .ok_or_else(|| corrupt("GIN analyzer owner references a missing table"))?;
            let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
            let parameters: BTreeMap<String, String> = serde_json::from_str(&row.parameters_json)?;
            let analyzer = parameters
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("analyzer"))
                .map(|(_, name)| name.trim());
            for field in keys.iter().filter_map(IndexKey::column) {
                if !table
                    .fts_fields
                    .read()
                    .iter()
                    .any(|current| current == field)
                {
                    table.fts_fields.write().push(field.to_owned());
                }
                if *table.columns_declared.read() {
                    Self::validate_table_analyzer_field(&table_name, &table, field)
                        .map_err(corrupt)?;
                }
                if let Some(name) = analyzer {
                    let key = (table_name.clone(), field.to_owned());
                    if owners
                        .insert(key, name.to_owned())
                        .is_some_and(|previous| previous != name)
                    {
                        return Err(corrupt("GIN definitions have competing analyzer owners"));
                    }
                }
            }
        }
        Ok(owners)
    }
}
