//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The table lifecycle retains completed metadata while native field owners prepare writable data.

use super::{
    Budgeted, ImageDecoder, MemoryTableDataInput, Metadata, RelationIdentity, StorageBackendError,
    StorageBackendResult, StorageReadControl, TableDestination,
};
use std::sync::Arc;
use uqa_analysis::CompiledAnalyzer;
use uqa_storage::{analyzer_binding::FieldAnalyzerBinding, inverted_index::AnalyzerDefault};

type Revisions<'a> = (&'a str, Arc<CompiledAnalyzer>, Arc<CompiledAnalyzer>);

pub(super) struct Prepared {
    pub name: Budgeted<RelationIdentity>,
    pub metadata: Metadata,
    pub data: Budgeted<Box<MemoryTableDataInput>>,
}

pub(super) fn input(
    name: Budgeted<RelationIdentity>,
    metadata: Metadata,
    destination: &TableDestination<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<ImageDecoder<Prepared, StorageBackendError>> {
    let fields = destination.fields.clone();
    let resources = destination.resources.clone();
    let registry = destination
        .state
        .memory_capture
        .as_ref()
        .expect("private memory destination")
        .clone();
    let maximum_value_bytes = destination.maximum_value_bytes;
    let temporary = destination.temporary.clone();
    let retained = control.clone();
    Ok(ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            let qualified = name.qualified_name_from(&mut input).await?;
            let revisions: for<'a> fn(
                (&'a str, &FieldAnalyzerBinding),
            ) -> StorageBackendResult<Revisions<'a>> = |(field, binding)| {
                Ok((
                    field,
                    binding.index.compiled.clone(),
                    binding.search.compiled.clone(),
                ))
            };
            let fields = fields
                .durable_fields_from(&qualified, &mut input)
                .await?
                .map(revisions);
            let default =
                AnalyzerDefault::from_definition(metadata.analyzer.clone(), resources, &retained)?;
            let memory = retained
                .memory()
                .reserve(size_of::<MemoryTableDataInput>())?;
            let data = MemoryTableDataInput::new_from(
                default,
                fields,
                &registry,
                &retained,
                maximum_value_bytes,
                &temporary,
                &mut input,
            )
            .await?;
            Ok(Prepared {
                name,
                metadata,
                data: Budgeted::new(Box::new(data), memory),
            })
        },
    )?)
}
