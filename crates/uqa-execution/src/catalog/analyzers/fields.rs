//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete field bindings precede compatibility labels and ordered owner checks.

use std::collections::BTreeMap;
use uqa_analysis::AnalyzerResources;
use uqa_storage::{
    AnalyzerBindingOwner, CatalogFacade, FieldAnalyzerBinding, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};

mod input;
pub use input::{FieldAnalyzerCatalogInput, RestoredFieldAnalyzers};

pub type FieldKey = (String, String);
pub type GinAnalyzerOwners = BTreeMap<FieldKey, String>;

pub struct CatalogFieldBindings {
    pub bindings: BTreeMap<FieldKey, FieldAnalyzerBinding>,
    pub labels: BTreeMap<FieldKey, Vec<(String, String)>>,
}

/// Ordinary migration and refresh retain their complete-family boundaries and native predicate order.
pub fn restore(
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
    let mut labels = BTreeMap::<FieldKey, Vec<(String, String)>>::new();
    for (table, field, phase, name) in catalog.load_table_field_analyzers()? {
        labels
            .entry((canonical_table(&table)?, field))
            .or_default()
            .push((phase, name));
    }
    for (key, binding) in &bindings {
        let label = labels.get(key).and_then(|labels| match labels.as_slice() {
            [(phase, name)] => Some((phase.as_str(), name.as_str())),
            _ => None,
        });
        validate_binding(binding, label, gin_owners.get(key).map(String::as_str))?;
    }
    Ok(CatalogFieldBindings { bindings, labels })
}

fn validate_binding(
    binding: &FieldAnalyzerBinding,
    label: Option<(&str, &str)>,
    gin_owner: Option<&str>,
) -> StorageBackendResult<()> {
    let (name, phase) = binding
        .last_assignment_ref()
        .unwrap_or(("", binding.phase_name()));
    if label != Some((phase, name)) {
        return Err(corrupt(
            "field analyzer binding disagrees with its catalog label",
        ));
    }
    if let Some(name) = gin_owner {
        if binding.owner != AnalyzerBindingOwner::Gin || binding.index.name.as_deref() != Some(name)
        {
            return Err(corrupt(
                "GIN definition conflicts with persisted analyzer ownership",
            ));
        }
    }
    Ok(())
}

fn canonical_table(name: &str) -> StorageBackendResult<String> {
    RelationIdentity::from_legacy_name(name)
        .map(|relation| relation.qualified_name())
        .map_err(corrupt)
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
