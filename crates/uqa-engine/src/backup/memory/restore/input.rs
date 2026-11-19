//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An unpublished table moves once from definition input to evaluated native data.

use super::{
    assemble,
    metadata::{header, Metadata, MetadataInput},
};
use crate::{state::StorageContext, TableState};
use std::{io::Read, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, ProductionControl},
    RelationIdentity, RelationName, RelationReference,
};
use uqa_execution::catalog::table_image::MemoryTableDataInput;
use uqa_storage::{
    analyzer_binding::registry::FieldAnalyzerCatalog,
    backup::{
        DecodedMemoryImageAddress, DecodedMemoryTableUnit, MEMORY_IMAGE_TABLE_METADATA_COUNT,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod preparation;
use preparation::Prepared;

pub(super) struct TableDestination<'a> {
    pub state: &'a StorageContext,
    pub fields: &'a FieldAnalyzerCatalog,
    pub resources: &'a AnalyzerResources,
    pub maximum_value_bytes: usize,
    pub window: ImageDecodeWindow,
    pub temporary: &'a DiskANNTemporaryBudget,
}

enum Stage {
    Header {
        name: Budgeted<RelationIdentity>,
        input: ImageDecoder<Budgeted<header::Header>, StorageBackendError>,
    },
    Metadata {
        name: Budgeted<RelationIdentity>,
        input: MetadataInput,
    },
    Preparing(ImageDecoder<Prepared, StorageBackendError>),
    Data(Prepared),
}

pub(super) struct TableInput {
    stage: Stage,
}

impl TableInput {
    pub(super) fn start(
        address: &DecodedMemoryImageAddress,
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
            stage: Stage::Header {
                name,
                input: header::input(*object, *generation, control)?,
            },
        })
    }

    pub(super) fn reading_header(&self) -> bool {
        matches!(self.stage, Stage::Header { .. })
    }

    /// The ordered image owner admits table identity and component membership. This consuming transition admits native payloads and registrations; no partially decoded table enters the destination registry.
    pub(super) fn read(
        self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        destination: &TableDestination<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Self, bool)> {
        control.check()?;
        let stage = match self.stage {
            Stage::Header { name, mut input } => {
                if !matches!(unit, DecodedMemoryTableUnit::Metadata(0)) {
                    return Err(invalid());
                }
                let Some(header) = input.advance(reader, destination.window)? else {
                    return Ok((
                        Self {
                            stage: Stage::Header { name, input },
                        },
                        false,
                    ));
                };
                Stage::Metadata {
                    name,
                    input: MetadataInput::new(header),
                }
            }
            Stage::Metadata {
                name,
                input: metadata,
            } => {
                let DecodedMemoryTableUnit::Metadata(slot) = unit else {
                    return Err(invalid());
                };
                let (metadata, complete) =
                    metadata.read(*slot, reader, destination.window, control)?;
                if !complete {
                    return Ok((
                        Self {
                            stage: Stage::Metadata {
                                name,
                                input: metadata,
                            },
                        },
                        false,
                    ));
                }
                if *slot + 1 == MEMORY_IMAGE_TABLE_METADATA_COUNT {
                    let metadata = metadata.finish(control)?;
                    return Ok((
                        Self {
                            stage: Stage::Preparing(preparation::input(
                                name,
                                metadata,
                                destination,
                                control,
                            )?),
                        },
                        false,
                    ));
                }
                Stage::Metadata {
                    name,
                    input: metadata,
                }
            }
            Stage::Preparing(mut input) => {
                if !matches!(unit, DecodedMemoryTableUnit::Metadata(slot) if *slot + 1 == MEMORY_IMAGE_TABLE_METADATA_COUNT)
                {
                    return Err(invalid());
                }
                let Some(prepared) = input.advance(reader, destination.window)? else {
                    return Ok((
                        Self {
                            stage: Stage::Preparing(input),
                        },
                        false,
                    ));
                };
                Stage::Data(prepared)
            }
            Stage::Data(mut prepared) => {
                let mut complete = false;
                prepared.data = prepared.data.try_map(|data| {
                    let (data, done) = data.advance(unit, reader, destination.window)?;
                    complete = done;
                    Ok::<_, StorageBackendError>(Box::new(data))
                })?;
                return Ok((
                    Self {
                        stage: Stage::Data(prepared),
                    },
                    complete,
                ));
            }
        };
        control.check()?;
        Ok((Self { stage }, true))
    }

    pub(super) fn finish(
        self,
        state: &StorageContext,
        control: &StorageReadControl,
    ) -> StorageBackendResult<(Budgeted<RelationIdentity>, Arc<TableState>)> {
        let Stage::Data(Prepared {
            name,
            metadata,
            data,
        }) = self.stage
        else {
            return Err(invalid());
        };
        let (data, _memory) = data.into_parts();
        let table = assemble(state, metadata, data.finish()?, control)?;
        Ok((name, table))
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("table image ended or supplied data before its definitions".into())
}
