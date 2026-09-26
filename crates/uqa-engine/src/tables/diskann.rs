//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind retained table state and the caller's resources to the owning `DiskANN` implementations.

use crate::{Engine, StorageBackendError, StorageBackendResult};
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{DiskANNIndexBinding, DiskANNIndexOptions, DiskANNMemoryIndex},
    vector_index::DiskANNIndexParams,
    CatalogIndexRow, VectorIndex, VectorIndexOpenMode,
};

impl Engine {
    pub(crate) fn build_catalog_diskann_index(
        &self,
        row: &CatalogIndexRow,
        field: &str,
        dimensions: u32,
        parameters: DiskANNIndexParams,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| StorageBackendError::Other("DiskANN table state disappeared".into()))?;
        let control = self
            .query_retention_control()
            .map_err(|error| StorageBackendError::backend("DiskANN resources", error))?;
        let options = DiskANNIndexOptions::for_parameters(parameters);
        let temporary = &self.session.diskann_temporary;
        if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(backend) = &self.storage.backend {
                return backend.diskann_index(
                    DiskANNIndexBinding {
                        table: &row.table_name,
                        field,
                        dimensions,
                        index: &row.relation,
                        resolver: Arc::new(
                            uqa_execution::catalog::index::diskann::DiskANNIndexIdentityResolver,
                        ),
                        control: &control,
                    },
                    options,
                    temporary,
                    mode,
                );
            }
        }
        if mode == VectorIndexOpenMode::Restore {
            let indexes = table.vector_indexes.read();
            return indexes
                .get(field)
                .filter(|index| index.index_kind() == "diskann")
                .ok_or_else(|| {
                    StorageBackendError::Other(
                        "memory DiskANN restore has no retained index".into(),
                    )
                })?
                .writable_snapshot();
        }
        let mut index = DiskANNMemoryIndex::new(dimensions, options, temporary, &control)?;
        let documents = table.document_store.read().snapshot()?;
        let columns = table.columns.read();
        let ty = columns
            .iter()
            .find(|column| column.name == field)
            .map(|column| &column.ty);
        uqa_execution::catalog::index::vectors::populate(&mut index, &*documents, field, ty)?;
        Ok(Box::new(index))
    }

    pub(crate) fn install_catalog_diskann_index(
        &self,
        row: &CatalogIndexRow,
        field: &str,
        dimensions: u32,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<()> {
        let index = self.build_catalog_diskann_index(
            row,
            field,
            dimensions,
            parameters,
            VectorIndexOpenMode::Create,
        )?;
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| StorageBackendError::Other("DiskANN table state disappeared".into()))?;
        table
            .vector_indexes
            .write()
            .live_mut()?
            .insert(field.to_owned(), index);
        Ok(())
    }

    pub(crate) fn retire_catalog_diskann_index(
        &self,
        row: &CatalogIndexRow,
        field: &str,
        dimensions: u32,
    ) -> StorageBackendResult<()> {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name)
            .map_err(StorageBackendError::Other)?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| StorageBackendError::Other("DiskANN table state disappeared".into()))?;
        if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(backend) = &self.storage.backend {
                let control = self
                    .query_retention_control()
                    .map_err(|error| StorageBackendError::backend("DiskANN resources", error))?;
                backend.retire_diskann_index(DiskANNIndexBinding {
                    table: &row.table_name,
                    field,
                    dimensions,
                    index: &row.relation,
                    resolver: Arc::new(
                        uqa_execution::catalog::index::diskann::DiskANNIndexIdentityResolver,
                    ),
                    control: &control,
                })?;
            }
        }
        Ok(())
    }
}
