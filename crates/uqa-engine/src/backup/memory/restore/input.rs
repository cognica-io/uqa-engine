//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An unpublished table moves once from definition input to evaluated native data.

use super::{
    assemble,
    metadata::{Metadata, MetadataInput},
};
use crate::{state::StorageContext, TableState};
use std::{io::Read, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    memory::{Budgeted, ProductionControl},
    RelationIdentity, RelationName, RelationReference,
};
use uqa_execution::catalog::table_image::MemoryTableDataInput;
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog,
    backup::{
        read_memory_table_metadata, DecodedMemoryImageAddress, DecodedMemoryTableUnit,
        MEMORY_IMAGE_TABLE_METADATA_COUNT,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    inverted_index::AnalyzerDefault,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(super) struct TableDestination<'a> {
    pub state: &'a StorageContext,
    pub fields: &'a FieldAnalyzerCatalog,
    pub resources: &'a AnalyzerResources,
    pub maximum_value_bytes: usize,
    pub temporary: &'a DiskANNTemporaryBudget,
}

enum Stage {
    Metadata(MetadataInput),
    Data(Metadata, Budgeted<Box<MemoryTableDataInput>>),
}

pub(super) struct TableInput {
    name: Budgeted<RelationIdentity>,
    stage: Stage,
}

impl TableInput {
    pub(super) fn open(
        address: &DecodedMemoryImageAddress,
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let DecodedMemoryImageAddress::Table {
            schema,
            relation,
            object,
            generation,
            unit: DecodedMemoryTableUnit::Metadata(0),
        } = address
        else {
            return Err(invalid());
        };
        let properties = read_memory_table_metadata(reader, control)?;
        if properties.properties.object_id != *object
            || properties.properties.storage_generation != *generation
        {
            return Err(StorageBackendError::Other(
                "table image address and definition identities disagree".into(),
            ));
        }
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let name = RelationReference {
            schema: Some(RelationName::decoded(schema)),
            name: RelationName::decoded(relation),
        }
        .identity_with_control(RelationName::decoded("public"), &production)?
        .into_budgeted()
        .expect("table input identity allowance");
        Ok(Self {
            name,
            stage: Stage::Metadata(MetadataInput::new(properties)),
        })
    }

    /// The ordered image owner admits table identity and component membership. This consuming transition admits native payloads and registrations; no partially decoded table enters the destination registry.
    pub(super) fn read(
        self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        destination: &TableDestination<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let stage = match self.stage {
            Stage::Metadata(metadata) => {
                let DecodedMemoryTableUnit::Metadata(slot) = unit else {
                    return Err(invalid());
                };
                let metadata = metadata.read(*slot, reader, control)?;
                if *slot + 1 == MEMORY_IMAGE_TABLE_METADATA_COUNT {
                    let metadata = metadata.finish(control)?;
                    let production = ProductionControl::new(
                        control.memory(),
                        control.cancellation(),
                        control.cancellation(),
                    );
                    let qualified = self.name.qualified_name_with_control(&production)?;
                    let fields = destination
                        .fields
                        .durable_fields_with_control(&qualified, control)?
                        .map(|(field, binding)| {
                            Ok((
                                field,
                                binding.index.compiled.clone(),
                                binding.search.compiled.clone(),
                            ))
                        });
                    let default = AnalyzerDefault::from_definition(
                        metadata.analyzer.clone(),
                        destination.resources.clone(),
                        control,
                    )?;
                    let memory = control
                        .memory()
                        .reserve(size_of::<MemoryTableDataInput>())?;
                    let data = MemoryTableDataInput::new(
                        default,
                        fields,
                        destination
                            .state
                            .memory_capture
                            .as_ref()
                            .expect("private memory destination"),
                        control,
                        destination.maximum_value_bytes,
                        destination.temporary,
                    )?;
                    Stage::Data(metadata, Budgeted::new(Box::new(data), memory))
                } else {
                    Stage::Metadata(metadata)
                }
            }
            Stage::Data(metadata, data) => {
                let (data, memory) = data.into_parts();
                Stage::Data(
                    metadata,
                    Budgeted::new(Box::new(data.read(unit, reader)?), memory),
                )
            }
        };
        control.check()?;
        Ok(Self {
            name: self.name,
            stage,
        })
    }

    pub(super) fn finish(
        self,
        state: &StorageContext,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Budgeted<RelationIdentity>, Arc<TableState>)> {
        let Stage::Data(metadata, data) = self.stage else {
            return Err(invalid());
        };
        let (data, _memory) = data.into_parts();
        let table = assemble(state, metadata, data.finish()?, control)?;
        Ok((self.name, table))
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("table image ended or supplied data before its definitions".into())
}
