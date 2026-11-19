//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored binding and label buffers survive ordered native assembly under one allowance.

#[cfg(test)]
use super::GinAnalyzerOwners;
use super::{corrupt, validate_binding, FieldKey};
use crate::catalog::analyzers::gin::GinAnalyzerLookup;
use std::{borrow::Borrow, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, OwnedMap, Produced, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_storage::{
    catalog::{
        analyzer_input::{
            CatalogAnalyzerBindingRow, CatalogAnalyzerInput, CatalogAnalyzerInputStep,
            CatalogAnalyzerLabelRow,
        },
        input::CatalogInputLimits,
    },
    read_control::StorageReadControl,
    CatalogFacade, FieldAnalyzerBinding, StorageBackendError, StorageBackendResult,
};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key(Produced<FieldKey>);

impl Borrow<FieldKey> for Key {
    fn borrow(&self) -> &FieldKey {
        &self.0
    }
}

type Labels = BudgetedVec<Budgeted<(String, String)>>;

/// Complete parsed records, including unmatched compatibility labels. The table installation owner consumes one original binding at a time and rejects any remaining records before completing restoration.
pub struct RestoredFieldAnalyzers {
    bindings: OwnedMap<Key, Budgeted<FieldAnalyzerBinding>>,
    labels: OwnedMap<Key, Labels>,
}

impl RestoredFieldAnalyzers {
    /// Transfer the original canonical key and binding allocations, retiring their already-validated compatibility label.
    pub fn take_binding(
        &mut self,
        key: &FieldKey,
    ) -> Option<(Budgeted<FieldKey>, Budgeted<FieldAnalyzerBinding>)> {
        let (key, binding) = self.bindings.remove_entry(key)?;
        self.labels.remove(&*key.0);
        let key = key
            .0
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("stored field keys retain admission"));
        Some((key, binding))
    }

    pub fn finish(self) -> StorageBackendResult<()> {
        if !self.bindings.is_empty() || !self.labels.is_empty() {
            return Err(corrupt(
                "table-field analyzer references a missing table, column, or physical FTS field",
            ));
        }
        Ok(())
    }
}

pub struct FieldAnalyzerCatalogInput {
    input: ImageDecoder<RestoredFieldAnalyzers, StorageBackendError>,
}

impl FieldAnalyzerCatalogInput {
    /// Retain both physical families from the caller-selected catalog view. GIN owners must already have completed their original earlier validation boundary; this input never re-resolves them.
    pub fn new(
        catalog: &dyn CatalogFacade,
        resources: Arc<AnalyzerResources>,
        limits: CatalogInputLimits,
        gin_owners: impl GinAnalyzerLookup + 'static,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let mut source = catalog.begin_analyzer_input(limits, control)?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let bindings = bindings(&mut source, &resources, &mut work).await?;
                let labels = labels(&mut source, &mut work).await?;
                for (key, binding) in &bindings {
                    work.checkpoint().await?;
                    let label = labels.get(&*key.0).and_then(|labels| match &labels[..] {
                        [label] => Some((label.0.as_str(), label.1.as_str())),
                        _ => None,
                    });
                    let gin = gin_owners.find_from(&key.0, &mut work).await?;
                    validate_binding(binding, label, gin)?;
                }
                retained.check()?;
                Ok(RestoredFieldAnalyzers { bindings, labels })
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<RestoredFieldAnalyzers>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn bindings(
    source: &mut CatalogAnalyzerInput,
    resources: &AnalyzerResources,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<OwnedMap<Key, Budgeted<FieldAnalyzerBinding>>> {
    let mut rows = BudgetedVec::new(work.memory);
    loop {
        work.checkpoint().await?;
        match source.advance()? {
            CatalogAnalyzerInputStep::Pending => (),
            CatalogAnalyzerInputStep::Binding(row) => rows.push(row)?,
            CatalogAnalyzerInputStep::BindingsComplete => break,
            _ => return Err(corrupt("analyzer input ended before complete bindings")),
        }
    }
    CatalogAnalyzerBindingRow::order_from(&mut rows, work).await?;
    let mut result = OwnedMap::new();
    let (rows, _row_memory) = rows.into_parts();
    for row in rows {
        work.checkpoint().await?;
        let (row, mut memory) = row.into_parts();
        let field_memory = memory.split(row.field.capacity());
        let json_memory = memory.split(row.binding_json.capacity());
        let table = Budgeted::new(row.table, memory);
        let field = Budgeted::new(row.field, field_memory);
        let json = Budgeted::new(row.binding_json, json_memory);
        let key = canonical_key(&table, field, work).await?;
        drop(table);
        let binding = FieldAnalyzerBinding::from_json_with_input(&json, resources, work).await?;
        drop(json);
        if result.insert_budgeted(key, binding, work.memory)?.is_some() {
            return Err(corrupt("duplicate durable field analyzer binding"));
        }
    }
    Ok(result)
}

async fn labels(
    source: &mut CatalogAnalyzerInput,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<OwnedMap<Key, Labels>> {
    let mut rows = BudgetedVec::new(work.memory);
    loop {
        work.checkpoint().await?;
        match source.advance()? {
            CatalogAnalyzerInputStep::Pending => (),
            CatalogAnalyzerInputStep::Label(row) => rows.push(row)?,
            CatalogAnalyzerInputStep::Complete => break,
            _ => {
                return Err(corrupt(
                    "analyzer input returned a binding after labels began",
                ))
            }
        }
    }
    CatalogAnalyzerLabelRow::order_from(&mut rows, work).await?;
    let mut result = OwnedMap::<Key, Labels>::new();
    let (rows, _row_memory) = rows.into_parts();
    for row in rows {
        work.checkpoint().await?;
        let (row, mut memory) = row.into_parts();
        let field_memory = memory.split(row.field.capacity());
        let mut label_memory = memory.split(row.phase.capacity());
        label_memory.absorb(memory.split(row.analyzer_name.capacity()));
        let table = Budgeted::new(row.table, memory);
        let field = Budgeted::new(row.field, field_memory);
        let label = Budgeted::new((row.phase, row.analyzer_name), label_memory);
        let key = canonical_key(&table, field, work).await?;
        drop(table);
        if let Some(labels) = result.get_mut(&key) {
            labels.push(label)?;
        } else {
            let mut labels = Labels::new(work.memory);
            labels.push(label)?;
            result.insert_budgeted(key, labels, work.memory)?;
        }
    }
    Ok(result)
}

async fn canonical_key(
    table: &str,
    field: Budgeted<String>,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Key> {
    let mut reference = match RelationIdentity::try_borrowed_reference(table) {
        Ok(reference) => reference,
        Err(error) => {
            let control = ProductionControl::new(work.memory, work.cancellation, work.cancellation);
            return Err(corrupt(
                control.format(format_args!("{error}"))?.into_parts().0,
            ));
        }
    };
    if reference.schema.is_none() {
        reference.schema = Some(RelationName::decoded("public"));
    }
    let (table, mut memory) = reference.render_from(work).await?.into_parts();
    let (field, field_memory) = field.into_parts();
    memory.absorb(field_memory);
    Ok(Key(Budgeted::new((table, field), memory).into()))
}

#[cfg(test)]
mod tests;
