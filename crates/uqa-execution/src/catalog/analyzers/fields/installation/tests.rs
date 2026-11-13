//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use uqa_analysis::AnalyzerResources;
use uqa_sql::catalog::{table_definition::TableDefinition, table_metadata::TableMetadata};
use uqa_storage::{catalog::input::CatalogInputLimits, CatalogFacade};

mod columns;
mod ordering;

#[derive(Clone)]
struct Table {
    fields: SharedTableMetadata<Vec<String>>,
    columns: SharedTableDefinition<Vec<ColumnDef>>,
    default: FieldAnalyzerBinding,
    fail: Option<String>,
    rebuild: bool,
    log: Arc<Mutex<Vec<String>>>,
}

impl AnalyzerTable for Table {
    fn persistence(&self) -> RelationPersistence {
        RelationPersistence::Permanent
    }
    fn fields(&self) -> SharedTableMetadata<Vec<String>> {
        self.fields.clone()
    }
    fn columns_declared(&self) -> bool {
        true
    }
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>> {
        self.columns.clone()
    }
    fn default_binding(&self, _: &str) -> StorageBackendResult<FieldAnalyzerBinding> {
        Ok(self.default.clone())
    }
    fn set_default(&self, _: AnalyzerDefinition) {
        self.log.lock().push("default".into());
    }
    fn install(&self, field: &str, _: &FieldAnalyzerBinding) -> StorageBackendResult<()> {
        self.log.lock().push(field.to_owned());
        if self.fail.as_deref() == Some(field) {
            return Err(corrupt(format!("forced field install {field}")));
        }
        Ok(())
    }
    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        Ok(self.rebuild)
    }
}

struct Tables(RelationIdentity, Table);
impl AnalyzerTables for Tables {
    type Table = Table;
    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Self::Table)> + Send {
        std::iter::once((&self.0, self.1.clone()))
    }
}

#[derive(Clone)]
struct Catalog {
    fields: Arc<Mutex<FieldAnalyzerCatalog>>,
    named: uqa_storage::analyzer_catalog::AnalyzerCatalog,
}
impl AnalyzerCatalogState for Catalog {
    fn fields(&self) -> FieldAnalyzerCatalog {
        self.fields.lock().clone()
    }
    fn named(&self, name: &str) -> Option<Arc<CompiledAnalyzer>> {
        self.named.get(name).cloned()
    }
    fn publish(&mut self, fields: FieldAnalyzerCatalog) {
        *self.fields.lock() = fields;
    }
}

fn binding(resources: &AnalyzerResources) -> FieldAnalyzerBinding {
    FieldAnalyzerBinding::unassigned(
        resources
            .compile(&uqa_analysis::whitespace_analyzer())
            .unwrap(),
        resources
            .compile(&uqa_analysis::keyword_analyzer())
            .unwrap(),
    )
}

fn table(binding: &FieldAnalyzerBinding) -> Table {
    Table {
        fields: TableMetadata::new(vec!["a".into(), "b".into()]).snapshot(),
        columns: TableDefinition::new(&vec![
            ColumnDef::nullable("a", uqa_sql::ast::ColumnType::Text),
            ColumnDef::nullable("b", uqa_sql::ast::ColumnType::Text),
        ])
        .unwrap()
        .shared(),
        default: binding.clone(),
        fail: None,
        rebuild: false,
        log: Arc::default(),
    }
}

fn tables(table: Table) -> Tables {
    Tables(RelationIdentity::new("public", "docs"), table)
}

fn saved(
    bindings: impl IntoIterator<Item = (&'static str, FieldAnalyzerBinding)>,
) -> CatalogFieldBindings {
    let mut saved = CatalogFieldBindings {
        bindings: BTreeMap::new(),
        labels: BTreeMap::new(),
    };
    for (field, binding) in bindings {
        let key = ("public.docs".into(), field.into());
        let (name, phase) = binding
            .last_assignment_ref()
            .unwrap_or(("", binding.phase_name()));
        saved
            .labels
            .insert(key.clone(), vec![(phase.into(), name.into())]);
        saved.bindings.insert(key, binding);
    }
    saved
}

fn retained(
    saved: CatalogFieldBindings,
    resources: Arc<AnalyzerResources>,
    control: &StorageReadControl,
) -> RestoredFieldAnalyzers {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = storage.catalog();
    for ((table, field), binding) in &saved.bindings {
        let (name, phase) = binding
            .last_assignment_ref()
            .unwrap_or(("", binding.phase_name()));
        catalog
            .replace_table_field_analyzer_binding(
                table,
                field,
                phase,
                name,
                &binding.to_json().unwrap(),
            )
            .unwrap();
    }
    for ((table, field), labels) in saved.labels {
        if !saved.bindings.contains_key(&(table.clone(), field.clone())) {
            for (phase, name) in labels {
                catalog
                    .save_table_field_analyzer(&table, &field, &phase, &name)
                    .unwrap();
            }
        }
    }
    let limits = CatalogInputLimits {
        maximum_key_bytes: 1 << 20,
        maximum_value_bytes: 1 << 20,
    };
    let mut input = super::super::FieldAnalyzerCatalogInput::new(
        &catalog,
        resources,
        limits,
        Arc::new(GinAnalyzerOwners::new()),
        control,
    )
    .unwrap();
    loop {
        if let Some(saved) = input.advance(1).unwrap() {
            return saved;
        }
    }
}

fn finish(input: &mut FieldAnalyzerInstallation) -> StorageBackendResult<FieldAnalyzerCatalog> {
    for _ in 0..1_000_000 {
        if let Some(result) = input.advance(1)? {
            return Ok(result);
        }
    }
    panic!("field installation stopped progressing")
}

#[test]
fn retained_field_installation_publishes_exact_sides_and_independent_later_mutations() {
    let resources = Arc::new(AnalyzerResources::default());
    let binding = binding(&resources);
    let table = table(&binding);
    let log = table.log.clone();
    let control = StorageReadControl::with_limit(16 << 20);
    let saved = retained(
        saved([("a", binding.clone()), ("b", binding)]),
        resources,
        &control,
    );
    let mutations = MemoryBudget::new(16 << 20);
    let names = uqa_storage::analyzer_catalog::AnalyzerCatalog::new(&mutations);
    let mut input =
        FieldAnalyzerInstallation::new(tables(table), saved, names, &mutations, &control).unwrap();
    assert!(input.advance(1).unwrap().is_none());
    let mut fields = finish(&mut input).unwrap();
    assert_eq!(*log.lock(), ["a", "b"]);
    assert_eq!(fields.len(), 2);
    let value = fields.get(&("public.docs".into(), "a".into())).unwrap();
    assert_eq!(value.index.compiled.analyze("A B").unwrap(), ["a", "b"]);
    assert_eq!(value.search.compiled.analyze("A B").unwrap(), ["A B"]);
    let old = fields.clone();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    fields.remove(&("public.docs".into(), "a".into())).unwrap();
    assert_eq!(old.len(), 2);
    assert_eq!(fields.len(), 1);
    drop((fields, old, input, occupied));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(mutations.used(), 0);
}
