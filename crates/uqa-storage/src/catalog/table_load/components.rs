//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored table fields transfer their original payload reservations without retaining discarded JSON.

use super::CatalogTable;
use crate::{
    RelationIdentity, RelationSecurityRow, StorageBackendError, StorageBackendResult, TableSchema,
    VectorFieldSchema,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, MemoryError},
};

#[derive(Debug)]
pub struct CatalogTableComponents {
    pub relation: Budgeted<RelationIdentity>,
    pub security: Budgeted<RelationSecurityRow>,
    pub object_id: [u8; 16],
    pub storage_generation: [u8; 16],
    pub analyzer_json: Budgeted<String>,
    pub fts_fields: Budgeted<Vec<String>>,
    pub vector_fields: Budgeted<Vec<VectorFieldSchema>>,
    pub columns_json: Budgeted<String>,
    pub constraints_json: Budgeted<String>,
}

impl CatalogTable {
    /// Transfer original buffers to independently owned fields through the enclosing input's work channel. The table envelope closes before its own fixed allowance is released; security retains its native map nodes and separate tuple-revision owners.
    pub async fn into_components_from(
        self,
        mut input: AsyncImageInput<'_>,
    ) -> StorageBackendResult<CatalogTableComponents> {
        input.checkpoint().await?;
        if !self.memory.budget().shares_allowance(input.memory) {
            return Err(StorageBackendError::Other(
                "table components require their original input allowance".into(),
            ));
        }
        let mut fields = bytes::<String>(self.fts_fields.capacity())?;
        for name in &self.fts_fields {
            input.checkpoint().await?;
            fields = add(fields, name.capacity())?;
        }
        let mut vectors = bytes::<VectorFieldSchema>(self.vector_fields.capacity())?;
        for field in &self.vector_fields {
            input.checkpoint().await?;
            vectors = add(vectors, field.field.capacity())?;
        }
        let relation_bytes = add(
            self.relation.schema.capacity(),
            self.relation.name.capacity(),
        )?;
        let (schema, mut memory) = {
            let (schema, memory) = self.into_budgeted().into_parts();
            (*schema, memory)
        };
        let TableSchema {
            relation,
            security,
            object_id,
            storage_generation,
            analyzer_json,
            fts_fields,
            vector_fields,
            columns_json,
            constraints_json,
        } = schema;
        drop(memory.split(size_of::<TableSchema>()));
        let relation = Budgeted::new(relation, memory.split(relation_bytes));
        let analyzer_bytes = analyzer_json.capacity();
        let analyzer_json = Budgeted::new(analyzer_json, memory.split(analyzer_bytes));
        let fts_fields = Budgeted::new(fts_fields, memory.split(fields));
        let vector_fields = Budgeted::new(vector_fields, memory.split(vectors));
        let columns_bytes = columns_json.capacity();
        let columns_json = Budgeted::new(columns_json, memory.split(columns_bytes));
        let constraints_bytes = constraints_json.capacity();
        let constraints_json = Budgeted::new(constraints_json, memory.split(constraints_bytes));
        Ok(CatalogTableComponents {
            relation,
            security: Budgeted::new(security, memory),
            object_id,
            storage_generation,
            analyzer_json,
            fts_fields,
            vector_fields,
            columns_json,
            constraints_json,
        })
    }
}

fn add(left: usize, right: usize) -> StorageBackendResult<usize> {
    left.checked_add(right)
        .ok_or_else(|| MemoryError::SizeOverflow.into())
}

fn bytes<T>(capacity: usize) -> StorageBackendResult<usize> {
    capacity
        .checked_mul(size_of::<T>())
        .ok_or_else(|| MemoryError::SizeOverflow.into())
}
