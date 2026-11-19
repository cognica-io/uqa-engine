//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage fields, SQL metadata and Analysis configuration share one unpublished input owner.

use super::sql_error;
use uqa_analysis::AnalyzerDefinition;
use uqa_core::{image_io::ImageDecoder, memory::Budgeted};
use uqa_sql::catalog::table_metadata::{TableMetadata, TableMetadataValue};
use uqa_storage::{
    backup::{read_memory_table_metadata_from, MemoryTableProperties},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(in crate::backup::memory::restore) struct Header {
    pub properties: MemoryTableProperties,
    pub fts_fields: TableMetadata<Vec<String>>,
    pub analyzer: AnalyzerDefinition,
    pub array_name: TableMetadata<Option<String>>,
}

pub(in crate::backup::memory::restore) fn input(
    object: [u8; 16],
    generation: [u8; 16],
    control: &StorageReadControl,
) -> StorageBackendResult<ImageDecoder<Budgeted<Header>, StorageBackendError>> {
    let admission = control.clone();
    Ok(ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let header = read_memory_table_metadata_from(reader.input()).await?;
            if header.properties.object_id != object
                || header.properties.storage_generation != generation
            {
                return Err(StorageBackendError::Other(
                    "table image address and definition identities disagree".into(),
                ));
            }
            header
                .try_map_async(|header| async move {
                    let analyzer =
                        AnalyzerDefinition::from_image_from(header.analyzer, reader.input())
                            .await
                            .map_err(|error| {
                                StorageBackendError::backend("memory table analyzer", error)
                            })?;
                    let fts_fields = metadata(header.fts_fields, &admission)?;
                    let array_name = metadata(header.row_type_array_name, &admission)?;
                    Ok(Header {
                        properties: header.properties,
                        fts_fields,
                        analyzer,
                        array_name,
                    })
                })
                .await
        },
    )?)
}

fn metadata<T: TableMetadataValue>(
    value: Budgeted<T>,
    control: &StorageReadControl,
) -> StorageBackendResult<TableMetadata<T>> {
    TableMetadata::from_admitted(value, control.cancellation()).map_err(sql_error)
}
