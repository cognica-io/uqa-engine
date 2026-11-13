//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field restoration preserves semantic, physical-installation and migration order.

use super::{corrupt, CatalogFieldBindings, FieldKey, GinAnalyzerOwners, RestoredFieldAnalyzers};
use std::{borrow::Borrow, collections::BTreeSet, sync::Arc};
use uqa_analysis::{AnalyzerDefinition, CompiledAnalyzer};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget},
    RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence},
    catalog::{table_definition::SharedTableDefinition, table_metadata::SharedTableMetadata},
};
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog, read_control::StorageReadControl,
    AnalyzerBindingOwner, FieldAnalyzerBinding, StorageBackendError, StorageBackendResult,
};

mod legacy;
mod work;
use work::Work;

#[cfg(test)]
mod tests;

/// Table adapters retain declarations and provide physical revision publication at its original boundary.
pub trait AnalyzerTable: Clone + Send + Sync {
    fn persistence(&self) -> RelationPersistence;
    fn fields(&self) -> SharedTableMetadata<Vec<String>>;
    fn columns_declared(&self) -> bool;
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>>;
    fn default_binding(&self, field: &str) -> StorageBackendResult<FieldAnalyzerBinding>;
    fn set_default(&self, definition: AnalyzerDefinition);
    fn install(&self, field: &str, binding: &FieldAnalyzerBinding) -> StorageBackendResult<()>;
    fn source_rebuild_required(&self) -> StorageBackendResult<bool>;
}

/// One selected table-membership generation; field and column generations are read during their original table visit.
pub trait AnalyzerTables: Send + Sync {
    type Table: AnalyzerTable;
    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Self::Table)> + Send;
}

/// Ordinary state adapters publish each successful field. Retained restoration owns a private root and returns it only after every table succeeds.
pub trait AnalyzerCatalogState: Send {
    fn named(&self, name: &str) -> Option<Arc<CompiledAnalyzer>>;
    fn fields(&self) -> FieldAnalyzerCatalog;
    fn publish(&mut self, fields: FieldAnalyzerCatalog);
}

#[derive(Default)]
pub struct BindingMigrations {
    pub bindings: Vec<(FieldKey, FieldAnalyzerBinding)>,
    pub rebuild_tables: BTreeSet<String>,
}

pub fn restore<T: AnalyzerTables>(
    tables: T,
    mut state: impl AnalyzerCatalogState,
    saved: CatalogFieldBindings,
    gin: &GinAnalyzerOwners,
    migrate: bool,
) -> StorageBackendResult<BindingMigrations> {
    work::inline(install(
        &tables,
        &mut state,
        legacy::Bindings::new(saved, gin, migrate),
        &mut (),
    ))
}

trait Bindings<T: AnalyzerTable, C: AnalyzerCatalogState>: Send {
    type Value: Borrow<FieldAnalyzerBinding> + Send;
    type Output;
    fn take(&mut self, key: &FieldKey, table: &T, state: &C) -> StorageBackendResult<Self::Value>;
    fn rebuild(&mut self, table: &str) -> StorageBackendResult<()>;
    fn finish(self) -> StorageBackendResult<Self::Output>;
}

async fn install<T, C, S, W>(
    tables: &T,
    state: &mut C,
    mut saved: S,
    work: &mut W,
) -> StorageBackendResult<S::Output>
where
    T: AnalyzerTables,
    C: AnalyzerCatalogState,
    S: Bindings<T::Table, C>,
    W: Work<Value = S::Value>,
{
    for (identity, table) in tables.tables() {
        work.checkpoint().await?;
        if table.persistence() == RelationPersistence::Temporary {
            continue;
        }
        let name = work.name(identity).await?;
        let fields = table.fields();
        for field in fields.iter() {
            work.checkpoint().await?;
            let key = work.key(&name, field).await?;
            let value = saved.take(&key, &table, state)?;
            let binding = value.borrow();
            if binding.owner == AnalyzerBindingOwner::Field
                && table.columns_declared()
                && (binding.index.name.is_some() || binding.search.name.is_some())
            {
                validate_declared_field(&table, &name, field, work).await?;
            }
            for side in [&binding.index, &binding.search] {
                work.checkpoint().await?;
                if let Some(name) = &side.name {
                    if state.named(name).is_none()
                        && !uqa_analysis::registry::contains_analyzer(name)
                    {
                        return Err(work.error(format_args!(
                            "field analyzer binding references missing name `{name}`"
                        )));
                    }
                }
            }
            let fields = work.prepare(state.fields(), &key, value).await?;
            let binding = fields
                .get(&key)
                .expect("prepared binding owns its original key");
            table.install(field, binding)?;
            state.publish(fields);
        }
        if table.source_rebuild_required()? {
            saved.rebuild(&name)?;
        }
    }
    saved.finish()
}

async fn validate_declared_field(
    table: &impl AnalyzerTable,
    name: &str,
    field: &str,
    work: &mut impl Work,
) -> StorageBackendResult<()> {
    work.column(&table.columns(), name, field).await?;
    let physical = table.fields();
    for candidate in physical.iter() {
        work.checkpoint().await?;
        if candidate == field {
            return Ok(());
        }
    }
    Err(work.error(format_args!(
        "set_table_analyzer: field `{name}`.`{field}` is not registered in the physical FTS index"
    )))
}

/// Legacy assignment and ordinary API resolution retain process-registry precedence over durable names.
pub fn resolve_analyzer_revision(
    name: &str,
    durable: impl FnOnce(&str) -> Option<Arc<CompiledAnalyzer>>,
) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
    let name = name.trim();
    if name.is_empty() {
        return Err(corrupt("analyzer name cannot be empty"));
    }
    if let Ok(analyzer) = uqa_analysis::registry::get_analyzer(name) {
        return Ok(analyzer.compile()?);
    }
    durable(name).ok_or_else(|| corrupt(format!("analyzer `{name}` is not registered")))
}

struct NativeBinding {
    key: Budgeted<FieldKey>,
    value: Budgeted<FieldAnalyzerBinding>,
}

impl Borrow<FieldAnalyzerBinding> for NativeBinding {
    fn borrow(&self) -> &FieldAnalyzerBinding {
        &self.value
    }
}

impl<T: AnalyzerTable, C: AnalyzerCatalogState> Bindings<T, C> for RestoredFieldAnalyzers {
    type Value = NativeBinding;
    type Output = ();
    fn take(&mut self, key: &FieldKey, _: &T, _: &C) -> StorageBackendResult<Self::Value> {
        let (key, value) = self
            .take_binding(key)
            .ok_or_else(|| corrupt("field analyzer catalog requires an initial migration"))?;
        Ok(NativeBinding { key, value })
    }
    fn rebuild(&mut self, _: &str) -> StorageBackendResult<()> {
        Err(corrupt(
            "positional index requires an initial source migration",
        ))
    }
    fn finish(self) -> StorageBackendResult<()> {
        self.finish()
    }
}

struct PrivateCatalog {
    named: uqa_storage::analyzer_catalog::AnalyzerCatalog,
    fields: FieldAnalyzerCatalog,
}

impl AnalyzerCatalogState for PrivateCatalog {
    fn named(&self, name: &str) -> Option<Arc<CompiledAnalyzer>> {
        self.named.get(name).cloned()
    }
    fn fields(&self) -> FieldAnalyzerCatalog {
        self.fields.clone()
    }
    fn publish(&mut self, fields: FieldAnalyzerCatalog) {
        self.fields = fields;
    }
}

pub struct FieldAnalyzerInstallation {
    input: ImageDecoder<FieldAnalyzerCatalog, StorageBackendError>,
}

impl FieldAnalyzerInstallation {
    pub fn new(
        tables: impl AnalyzerTables + 'static,
        saved: RestoredFieldAnalyzers,
        named: uqa_storage::analyzer_catalog::AnalyzerCatalog,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mutations = mutations.clone();
        let mut state = PrivateCatalog {
            named,
            fields: FieldAnalyzerCatalog::new(control.memory()),
        };
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                install(&tables, &mut state, saved, &mut work).await?;
                Ok(state.fields.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<FieldAnalyzerCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
