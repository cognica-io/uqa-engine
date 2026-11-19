//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial migration retains compatibility phase order and the original source-rebuild boundary.

use super::{
    corrupt, resolve_analyzer_revision, AnalyzerCatalogState, AnalyzerTable, BindingMigrations,
    CatalogFieldBindings, FieldKey, GinAnalyzerOwners,
};
use std::{collections::BTreeSet, sync::Arc};
use uqa_analysis::{AnalyzerDefinition, CompiledAnalyzer};
use uqa_storage::{
    AnalyzerBindingOwner, AnalyzerPhase, FieldAnalyzerBinding, StorageBackendResult,
};

pub(super) struct Bindings<'a> {
    saved: CatalogFieldBindings,
    gin: &'a GinAnalyzerOwners,
    migrate: bool,
    migrations: BindingMigrations,
}

impl<'a> Bindings<'a> {
    pub(super) fn new(
        saved: CatalogFieldBindings,
        gin: &'a GinAnalyzerOwners,
        migrate: bool,
    ) -> Self {
        Self {
            saved,
            gin,
            migrate,
            migrations: BindingMigrations::default(),
        }
    }
}

impl<T: AnalyzerTable, C: AnalyzerCatalogState> super::Bindings<T, C> for Bindings<'_> {
    type Value = FieldAnalyzerBinding;
    type Output = BindingMigrations;
    fn take(&mut self, key: &FieldKey, table: &T, state: &C) -> StorageBackendResult<Self::Value> {
        if let Some(binding) = self.saved.bindings.remove(key) {
            self.saved.labels.remove(key);
            return Ok(binding);
        }
        if !self.migrate {
            return Err(corrupt(
                "field analyzer catalog requires an initial migration",
            ));
        }
        let binding = legacy(
            key,
            table,
            state,
            self.saved.labels.remove(key).unwrap_or_default(),
            self.gin.get(key),
        )?;
        self.migrations
            .bindings
            .push((key.clone(), binding.clone()));
        self.migrations.rebuild_tables.insert(key.0.clone());
        Ok(binding)
    }
    fn rebuild(&mut self, name: &str) -> StorageBackendResult<()> {
        if !self.migrate {
            return Err(corrupt(
                "positional index requires an initial source migration",
            ));
        }
        self.migrations.rebuild_tables.insert(name.to_owned());
        Ok(())
    }
    fn finish(self) -> StorageBackendResult<Self::Output> {
        if !self.saved.bindings.is_empty() || !self.saved.labels.is_empty() {
            return Err(corrupt(
                "table-field analyzer references a missing table, column, or physical FTS field",
            ));
        }
        Ok(self.migrations)
    }
}

fn legacy<T: AnalyzerTable, C: AnalyzerCatalogState>(
    key: &FieldKey,
    table: &T,
    state: &C,
    labels: Vec<(String, String)>,
    gin: Option<&String>,
) -> StorageBackendResult<FieldAnalyzerBinding> {
    let mut old = Vec::new();
    let mut seen = BTreeSet::new();
    for (phase, name) in labels {
        let (phase_name, phase) = AnalyzerPhase::normalize(&phase).map_err(corrupt)?;
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
    let explicit = gin.map(String::as_str).or_else(|| {
        old.iter()
            .find(|(phase, _)| {
                *phase == AnalyzerPhase::Both
                    || (*phase == AnalyzerPhase::Index && seen.contains("search"))
            })
            .map(|(_, name)| name.as_str())
    });
    let mut binding = if let Some(name) = explicit {
        let revision = resolve(state, name)?;
        FieldAnalyzerBinding::unassigned(revision.clone(), revision)
    } else {
        let binding = match state.fields().get(key) {
            Some(binding) => binding.clone(),
            None => table.default_binding(&key.1)?,
        };
        table.set_default(AnalyzerDefinition::new(
            binding.index.compiled.descriptor().configuration()?,
        ));
        binding
    };
    for (phase, name) in old {
        let revision = resolve(state, &name)?;
        binding = binding.assigned(&name, revision, phase, AnalyzerBindingOwner::Field);
    }
    if let Some(name) = gin {
        if binding.index.name.is_some()
            && (binding.index.name.as_ref() != Some(name)
                || binding.search.name.as_ref() != Some(name)
                || binding.last_phase != AnalyzerPhase::Both)
        {
            return Err(corrupt(
                "legacy GIN and field assignments have competing analyzer owners",
            ));
        }
        binding = binding.assigned(
            name,
            resolve(state, name)?,
            AnalyzerPhase::Both,
            AnalyzerBindingOwner::Gin,
        );
    }
    Ok(binding)
}

fn resolve(
    state: &impl AnalyzerCatalogState,
    name: &str,
) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
    resolve_analyzer_revision(name, |name| state.named(name))
}
