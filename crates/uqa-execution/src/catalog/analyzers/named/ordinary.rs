//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial migration and ordinary refresh preserve complete named-family validation before publication.

use super::{corrupt, AnalyzerResources, CatalogFacade, CompiledAnalyzer, StorageBackendResult};
use std::{collections::BTreeMap, sync::Arc};

pub struct NamedAnalyzerRevisions {
    pub current: BTreeMap<String, Arc<CompiledAnalyzer>>,
    pub pending: Vec<(String, Arc<CompiledAnalyzer>)>,
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    resources: &AnalyzerResources,
    allow_migration: bool,
    parse_legacy: impl Fn(&str, &str) -> Result<uqa_analysis::Analyzer, String>,
) -> StorageBackendResult<NamedAnalyzerRevisions> {
    let mut descriptors = BTreeMap::new();
    for (name, descriptor) in catalog.load_analyzer_descriptors()? {
        super::validate_descriptor_name(&name)?;
        if descriptors.insert(name, descriptor).is_some() {
            return Err(corrupt("duplicate named analyzer descriptor"));
        }
    }
    let mut names = BTreeMap::<String, Arc<CompiledAnalyzer>>::new();
    let mut migrate_names = Vec::new();
    for (name, configuration) in catalog.load_analyzers()? {
        super::validate_definition_name(&name)?;
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
            if !allow_migration {
                return Err(corrupt("analyzer catalog requires an initial migration"));
            }
            let config = parse_legacy(&name, &configuration).map_err(corrupt)?;
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
