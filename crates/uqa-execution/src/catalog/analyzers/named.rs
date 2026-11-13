//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete stored named revisions precede paired native catalog publication.

use std::sync::Arc;
use uqa_analysis::{AnalyzerDiagnostic, AnalyzerResources, CompiledAnalyzer};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    json::JsonReadError,
    memory::{Budgeted, MemoryBudget, OwnedMap, Produced},
};
use uqa_storage::{
    analyzer_catalog::AnalyzerCatalog,
    catalog::{
        input::CatalogInputLimits,
        text_input::{CatalogTextFamily, CatalogTextRow},
        text_load::CatalogTextRows,
    },
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

mod ordinary;
pub use ordinary::{restore, NamedAnalyzerRevisions};

pub struct NamedAnalyzerCatalogInput {
    input: ImageDecoder<AnalyzerCatalog, StorageBackendError>,
}
impl NamedAnalyzerCatalogInput {
    /// The caller retains the selected catalog transaction. All descriptor rows precede descriptor-name checks; all definition rows precede definition checks. Only a complete native catalog transfers to its receiving mutation allowance.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        resources: Arc<AnalyzerResources>,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let descriptor_rows = rows(
                    catalog.as_ref(),
                    CatalogTextFamily::AnalyzerDescriptors,
                    limits,
                    &retained,
                    &mut work,
                )
                .await?;
                let mut descriptors = OwnedMap::<String, Budgeted<String>>::new();
                {
                    let (descriptor_rows, _row_memory) = descriptor_rows.into_parts();
                    for row in descriptor_rows {
                        work.checkpoint().await?;
                        validate_descriptor_name(&row.name)?;
                        let (name, value) = fields(row.into_budgeted());
                        if descriptors.insert_admitted_key(name, value)?.is_some() {
                            return Err(corrupt("duplicate named analyzer descriptor"));
                        }
                    }
                }
                let definitions = rows(
                    catalog.as_ref(),
                    CatalogTextFamily::Analyzers,
                    limits,
                    &retained,
                    &mut work,
                )
                .await?;
                let mut revisions = OwnedMap::<Produced<String>, Arc<CompiledAnalyzer>>::new();
                {
                    let (definitions, _row_memory) = definitions.into_parts();
                    for row in definitions {
                        work.checkpoint().await?;
                        validate_definition_name(&row.name)?;
                        let descriptor =
                            descriptors.remove(row.name.as_str()).ok_or_else(|| {
                                corrupt("analyzer catalog requires an initial migration")
                            })?;
                        let compiled = resources
                            .restore_json_with_input(&descriptor, &mut work)
                            .await?;
                        drop(descriptor);
                        let saved = AnalyzerDiagnostic::read_from(&row.value, &mut work)
                            .await
                            .map_err(json_error)?;
                        let configuration = compiled
                            .descriptor()
                            .configuration_with_input(&mut work)
                            .await?;
                        if !saved
                            .matches_from(&configuration, &mut work)
                            .await
                            .map_err(json_error)?
                        {
                            return Err(corrupt("named analyzer descriptor disagrees with its diagnostic configuration"));
                        }
                        drop(configuration);
                        let (name, value) = fields(row.into_budgeted());
                        drop(value);
                        if revisions
                            .insert_budgeted(name.into(), compiled, retained.memory())?
                            .is_some()
                        {
                            return Err(corrupt("duplicate catalog analyzer definition"));
                        }
                    }
                }
                if !descriptors.is_empty() {
                    return Err(corrupt(
                        "analyzer descriptor references a missing named definition",
                    ));
                }
                let mut names = AnalyzerCatalog::new(retained.memory());
                for (name, compiled) in revisions {
                    work.checkpoint().await?;
                    let name = name
                        .into_budgeted()
                        .unwrap_or_else(|_| unreachable!("stored analyzer names retain admission"));
                    names = names
                        .insert_restored_from(name, compiled, &mut work)
                        .await?;
                }
                retained.check()?;
                Ok(names.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<AnalyzerCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn rows(
    catalog: &dyn CatalogFacade,
    family: CatalogTextFamily,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<CatalogTextRows> {
    work.checkpoint().await?;
    let mut input = catalog.begin_text_load(family, limits, control)?;
    loop {
        work.checkpoint().await?;
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
}
fn fields(row: Budgeted<CatalogTextRow>) -> (Budgeted<String>, Budgeted<String>) {
    let (row, mut memory) = row.into_parts();
    let name_memory = memory.split(row.name.capacity());
    (
        Budgeted::new(row.name, name_memory),
        Budgeted::new(row.value, memory),
    )
}

fn validate_descriptor_name(name: &str) -> StorageBackendResult<()> {
    if uqa_analysis::registry::is_builtin_analyzer(name) {
        return Err(corrupt(format!(
            "catalog analyzer `{name}` conflicts with a built-in analyzer"
        )));
    }
    Ok(())
}
fn validate_definition_name(name: &str) -> StorageBackendResult<()> {
    if name.is_empty()
        || name.chars().next().is_some_and(char::is_whitespace)
        || name.chars().next_back().is_some_and(char::is_whitespace)
    {
        return Err(corrupt("catalog analyzer has an invalid name"));
    }
    validate_descriptor_name(name)
}
fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(
            "invalid named analyzer diagnostic JSON",
        )),
    }
}

#[cfg(test)]
mod tests;
