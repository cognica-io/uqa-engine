//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore exact analyzer revisions and migrate legacy names within the owning catalog transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_analysis::{AnalyzerResources, CompiledAnalyzer};
use uqa_sql::ast::{IndexKey, RelationPersistence};
use uqa_storage::{
    AnalyzerBindingOwner, AnalyzerPhase, CatalogFacade, FieldAnalyzerBinding, RelationIdentity,
    StorageBackendError, StorageBackendResult,
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

type GinAnalyzerOwners = BTreeMap<(String, String), String>;

impl Engine {
    pub(crate) fn current_field_analyzer_binding(
        &self,
        table_name: &str,
        table: &TableState,
        field: &str,
    ) -> Result<FieldAnalyzerBinding, String> {
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
            index
                .index_analyzer_revision(field)
                .map_err(|error| error.to_string())?,
            index
                .search_analyzer_revision(field)
                .map_err(|error| error.to_string())?,
        ))
    }

    pub(crate) fn persist_field_analyzer_binding(
        &self,
        table: &str,
        field: &str,
        binding: &FieldAnalyzerBinding,
    ) -> Result<(), String> {
        let json = binding.to_json().map_err(|error| error.to_string())?;
        if self
            .try_table(table)
            .map_err(|error| error.to_string())?
            .is_some_and(|table| table.persistence == RelationPersistence::Temporary)
        {
            return Ok(());
        }
        if let Some(catalog) = &self.storage.catalog {
            let name = binding
                .last_assignment()
                .map_or_else(String::new, |(name, _)| name);
            catalog
                .replace_table_field_analyzer_binding(
                    table,
                    field,
                    binding.phase_name(),
                    &name,
                    &json,
                )
                .map_err(|error| format!("persist table analyzer `{table}`.`{field}`: {error}"))?;
        }
        Ok(())
    }

    pub(crate) fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let resources = AnalyzerResources::default();
        let names = load_named_revisions(catalog, &resources, mode)?;
        self.durable.named_analyzers.write().extend(names.current);
        let gin_owners = self.restore_gin_analyzer_owners(catalog)?;
        let mut saved = load_field_bindings(catalog, &resources, &gin_owners)?;
        let migrations = self.install_catalog_field_analyzers(&mut saved, &gin_owners, mode)?;
        // Every descriptor and owner is validated before rebuilding sources or publishing analyzer metadata.
        for table_name in migrations.rebuild_tables {
            let table = self
                .try_table(&table_name)?
                .ok_or_else(|| corrupt("missing analyzer migration table"))?;
            Self::rebuild_fts_index(&table).map_err(corrupt)?;
            self.try_save_table_schema(&table_name, &table)?;
        }
        for (name, compiled) in names.pending {
            let configuration = serde_json::to_string(&compiled.descriptor().configuration()?)?;
            catalog.save_analyzer_revision(
                &name,
                &configuration,
                compiled.descriptor().canonical_json(),
            )?;
        }
        for ((table, field), binding) in migrations.bindings {
            self.persist_field_analyzer_binding(&table, &field, &binding)
                .map_err(corrupt)?;
        }
        Ok(())
    }

    fn install_catalog_field_analyzers(
        &self,
        saved: &mut CatalogFieldBindings,
        gin_owners: &GinAnalyzerOwners,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<BindingMigrations> {
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(_, table)| table.persistence != RelationPersistence::Temporary)
            .map(|(name, table)| (name.qualified_name(), table.clone()))
            .collect::<Vec<_>>();
        let mut migrations = BindingMigrations::default();
        for (table_name, table) in &tables {
            for field in table.fts_fields() {
                let key = (table_name.clone(), field.clone());
                let binding = if let Some(binding) = saved.bindings.remove(&key) {
                    saved.labels.remove(&key);
                    binding
                } else {
                    if !mode.allows_migration() {
                        return Err(corrupt(
                            "field analyzer catalog requires an initial migration",
                        ));
                    }
                    let binding = self.legacy_field_analyzer_binding(
                        table_name,
                        table,
                        &field,
                        saved.labels.remove(&key).unwrap_or_default(),
                        gin_owners.get(&key),
                    )?;
                    migrations.bindings.push((key, binding.clone()));
                    migrations.rebuild_tables.insert(table_name.clone());
                    binding
                };
                let named_field_assignment = binding.owner == AnalyzerBindingOwner::Field
                    && (binding.index.name.is_some() || binding.search.name.is_some());
                if *table.columns_declared.read() || named_field_assignment {
                    Self::validate_table_analyzer_field(table_name, table, &field)
                        .map_err(corrupt)?;
                }
                for side in [&binding.index, &binding.search] {
                    if let Some(name) = &side.name {
                        if !self.durable.named_analyzers.read().contains_key(name)
                            && crate::analyzer_registry::get_analyzer(name).is_err()
                        {
                            return Err(corrupt(format!(
                                "field analyzer binding references missing name `{name}`"
                            )));
                        }
                    }
                }
                binding.install(&field, table.inverted_index.write().as_mut())?;
                self.durable
                    .table_field_analyzers
                    .write()
                    .insert((table_name.clone(), field), binding);
            }
            if table.inverted_index.read().source_rebuild_required()? {
                if !mode.allows_migration() {
                    return Err(corrupt(
                        "positional index requires an initial source migration",
                    ));
                }
                migrations.rebuild_tables.insert(table_name.clone());
            }
        }
        if !saved.bindings.is_empty() || !saved.labels.is_empty() {
            return Err(corrupt(
                "table-field analyzer references a missing table, column, or physical FTS field",
            ));
        }
        Ok(migrations)
    }

    fn legacy_field_analyzer_binding(
        &self,
        table_name: &str,
        table: &TableState,
        field: &str,
        labels: Vec<(String, String)>,
        gin_owner: Option<&String>,
    ) -> StorageBackendResult<FieldAnalyzerBinding> {
        let mut old = Vec::new();
        let mut seen = BTreeSet::new();
        for (phase, name) in labels {
            let (phase_name, phase) = crate::normalize_analyzer_phase(&phase).map_err(corrupt)?;
            if !seen.insert(phase_name) {
                return Err(corrupt("ambiguous legacy analyzer phases"));
            }
            old.push((phase, name));
        }
        old.sort_by_key(|(phase, _)| match phase {
            AnalyzerPhase::Both => 0,
            AnalyzerPhase::Index => 1,
            AnalyzerPhase::Search => 2,
        });
        let explicit_base = gin_owner.map(String::as_str).or_else(|| {
            old.iter()
                .find(|(phase, _)| {
                    *phase == AnalyzerPhase::Both
                        || (*phase == AnalyzerPhase::Index && seen.contains("search"))
                })
                .map(|(_, name)| name.as_str())
        });
        let mut binding = if let Some(name) = explicit_base {
            let revision = self.resolve_analyzer_revision(name).map_err(corrupt)?;
            FieldAnalyzerBinding::unassigned(revision.clone(), revision)
        } else {
            let binding = self
                .current_field_analyzer_binding(table_name, table, field)
                .map_err(corrupt)?;
            *table.analyzer.write() = binding.index.compiled.descriptor().configuration()?;
            binding
        };
        for (phase, name) in old {
            let compiled = self.resolve_analyzer_revision(&name).map_err(corrupt)?;
            binding = binding.assigned(&name, compiled, phase, AnalyzerBindingOwner::Field);
        }
        if let Some(name) = gin_owner {
            if binding.index.name.is_some()
                && (binding.index.name.as_ref() != Some(name)
                    || binding.search.name.as_ref() != Some(name)
                    || binding.last_phase != AnalyzerPhase::Both)
            {
                return Err(corrupt(
                    "legacy GIN and field assignments have competing analyzer owners",
                ));
            }
            let compiled = self.resolve_analyzer_revision(name).map_err(corrupt)?;
            binding = binding.assigned(
                name,
                compiled,
                AnalyzerPhase::Both,
                AnalyzerBindingOwner::Gin,
            );
        }
        Ok(binding)
    }

    pub(crate) fn initialize_table_analyzer_bindings(
        &self,
        name: &str,
        table: &TableState,
        default_revision: Option<Arc<CompiledAnalyzer>>,
    ) -> StorageBackendResult<crate::TableFieldAnalyzerRegistry> {
        let mut bindings = BTreeMap::new();
        if let Some(revision) = default_revision {
            for field in table.fts_fields() {
                let binding = FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone());
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
                bindings.insert((name.to_owned(), field), binding);
            }
        }
        Ok(bindings)
    }

    fn restore_gin_analyzer_owners(
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

type FieldKey = (String, String);

struct NamedAnalyzerRevisions {
    current: BTreeMap<String, Arc<CompiledAnalyzer>>,
    pending: Vec<(String, Arc<CompiledAnalyzer>)>,
}

struct CatalogFieldBindings {
    bindings: BTreeMap<FieldKey, FieldAnalyzerBinding>,
    labels: BTreeMap<FieldKey, Vec<(String, String)>>,
}

#[derive(Default)]
struct BindingMigrations {
    bindings: Vec<(FieldKey, FieldAnalyzerBinding)>,
    rebuild_tables: BTreeSet<String>,
}

fn load_named_revisions(
    catalog: &dyn CatalogFacade,
    resources: &AnalyzerResources,
    mode: CatalogRestoreMode,
) -> StorageBackendResult<NamedAnalyzerRevisions> {
    let mut descriptors = BTreeMap::new();
    for (name, descriptor) in catalog.load_analyzer_descriptors()? {
        if descriptors.insert(name, descriptor).is_some() {
            return Err(corrupt("duplicate named analyzer descriptor"));
        }
    }
    let mut names = BTreeMap::<String, Arc<CompiledAnalyzer>>::new();
    let mut migrate_names = Vec::new();
    for (name, configuration) in catalog.load_analyzers()? {
        if name.is_empty() || name.trim() != name {
            return Err(corrupt("catalog analyzer has an invalid name"));
        }
        let compiled = if let Some(descriptor) = descriptors.remove(&name) {
            let compiled = resources.restore_json(&descriptor)?;
            let saved: serde_json::Value = serde_json::from_str(&configuration)?;
            if saved != serde_json::to_value(compiled.descriptor().configuration()?)? {
                return Err(corrupt(
                    "named analyzer descriptor disagrees with its diagnostic configuration",
                ));
            }
            compiled
        } else {
            if !mode.allows_migration() {
                return Err(corrupt("analyzer catalog requires an initial migration"));
            }
            let config = crate::parse_analyzer_config(&name, &configuration).map_err(corrupt)?;
            let compiled = resources.compile(&config)?;
            migrate_names.push((name.clone(), compiled.clone()));
            compiled
        };
        if names.insert(name, compiled).is_some() {
            return Err(corrupt("duplicate catalog analyzer definition"));
        }
    }
    if !descriptors.is_empty() {
        return Err(corrupt(
            "analyzer descriptor references a missing named definition",
        ));
    }
    Ok(NamedAnalyzerRevisions {
        current: names,
        pending: migrate_names,
    })
}

fn load_field_bindings(
    catalog: &dyn CatalogFacade,
    resources: &AnalyzerResources,
    gin_owners: &GinAnalyzerOwners,
) -> StorageBackendResult<CatalogFieldBindings> {
    let mut bindings = BTreeMap::new();
    for (table, field, json) in catalog.load_table_field_analyzer_bindings()? {
        let key = (canonical_table(&table)?, field);
        let binding = FieldAnalyzerBinding::from_json(&json, resources)?;
        if bindings.insert(key, binding).is_some() {
            return Err(corrupt("duplicate durable field analyzer binding"));
        }
    }
    let mut labels = BTreeMap::<(String, String), Vec<(String, String)>>::new();
    for (table, field, phase, name) in catalog.load_table_field_analyzers()? {
        labels
            .entry((canonical_table(&table)?, field))
            .or_default()
            .push((phase, name));
    }
    for (key, binding) in &bindings {
        let expected = binding
            .last_assignment()
            .unwrap_or_else(|| (String::new(), binding.phase_name().to_owned()));
        if labels.get(key) != Some(&vec![(expected.1, expected.0)]) {
            return Err(corrupt(
                "field analyzer binding disagrees with its catalog label",
            ));
        }
        if let Some(name) = gin_owners.get(key) {
            if binding.owner != AnalyzerBindingOwner::Gin
                || binding.index.name.as_ref() != Some(name)
            {
                return Err(corrupt(
                    "GIN definition conflicts with persisted analyzer ownership",
                ));
            }
        }
    }
    Ok(CatalogFieldBindings { bindings, labels })
}
