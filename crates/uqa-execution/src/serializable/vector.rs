//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the candidate corpus of an executing vector search, independently of physical ANN structures.

use std::{ops::Bound::Included, sync::Arc};

use uqa_core::{memory::Budgeted, DocId, PostingList};
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{
    mvcc::{SerializableKeySpace, SerializablePredicate},
    vector_index::validate_vector_values,
    StorageBackendError, StorageBackendResult, VectorIndex,
};

use super::{field::prefix as field_prefix, SerializableRelationRead, SerializableWrites};
use crate::storage_errors::storage_error;

/// Retain the original participant with the already-selected index snapshot. Metadata inspection and unused operator contexts do not register a read.
pub fn observe_snapshot(
    read: Option<&SerializableRelationRead>,
    columns: &[ColumnDef],
    field: &str,
    index: Arc<dyn VectorIndex>,
) -> StorageBackendResult<Arc<dyn VectorIndex>> {
    let Some(read) = read else {
        return Ok(index);
    };
    Ok(Arc::new(ObservedVectorIndex {
        index,
        observation: VectorObservation::bind(read, columns, field)?,
    }))
}

/// Execute against a retained live index guard without copying its corpus just to attach an observer.
pub fn search_knn(
    index: &dyn VectorIndex,
    read: Option<&SerializableRelationRead>,
    columns: &[ColumnDef],
    field: &str,
    query: &[f32],
    k: usize,
) -> StorageBackendResult<PostingList> {
    if let Some(read) = read.filter(|_| k != 0) {
        validate_vector_values(index.dimensions(), query)?;
        VectorObservation::bind(read, columns, field)?.observe()?;
    }
    index.search_knn(query, k)
}

#[derive(Clone)]
struct ObservedVectorIndex {
    index: Arc<dyn VectorIndex>,
    observation: VectorObservation,
}

#[derive(Clone)]
struct VectorObservation {
    read: SerializableRelationRead,
    upper: Arc<Budgeted<Vec<u8>>>,
    prefix_len: usize,
}

impl VectorObservation {
    fn bind(
        read: &SerializableRelationRead,
        columns: &[ColumnDef],
        field: &str,
    ) -> StorageBackendResult<Self> {
        let mut upper = field_prefix(columns, field, &read.control)?;
        let prefix_len = upper.len();
        upper.extend_from_slice(&DocId::MAX.to_be_bytes())?;
        let (upper, memory) = upper.into_parts();
        Ok(Self {
            read: read.clone(),
            upper: Budgeted::new(upper, memory).into_shared()?,
            prefix_len,
        })
    }

    fn observe(&self) -> StorageBackendResult<()> {
        // Exact search examines the field's candidates; ANN selection can also change when any canonical candidate changes. An empty result retains the same phantom range.
        self.read
            .context
            .observe_read(
                SerializablePredicate::range(
                    self.read.object,
                    SerializableKeySpace::Vectors,
                    Included(&self.upper[..self.prefix_len]),
                    Included(&self.upper),
                ),
                &self.read.control,
            )
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)
    }
}

impl VectorIndex for ObservedVectorIndex {
    fn dimensions(&self) -> u32 {
        self.index.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        self.index.index_kind()
    }
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.index.contains_document(doc_id)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.index.count()
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions(), query)?;
        if k != 0 {
            self.observation.observe()?;
        }
        self.index.search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions(), query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(
                "vector similarity threshold must be finite".into(),
            ));
        }
        self.observation.observe()?;
        self.index.search_threshold(query, threshold)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained serializable vector snapshot".into())
}

/// Already-evaluated canonical replacement or deletion; observation never replays an expression or index construction.
pub enum VectorChange<'a> {
    Single(&'a [f32]),
    Tensor(&'a [Vec<f32>]),
    Delete,
}

/// Record a canonical document replacement before publication. Deleting an absent candidate is not a write; nonempty replacements are write intents even when the values compare equal, as with ordinary row/index replacement.
pub fn observe_write(
    writes: &dyn SerializableWrites,
    table: &str,
    columns: &[ColumnDef],
    field: &str,
    index: &dyn VectorIndex,
    doc_id: DocId,
    change: VectorChange<'_>,
) -> Result<(), SQLError> {
    let Some(read) = SerializableRelationRead::for_mutation(writes, table)? else {
        return Ok(());
    };
    let nonempty = (|| -> StorageBackendResult<bool> {
        match change {
            VectorChange::Single(vector) => {
                validate_vector_values(index.dimensions(), vector)?;
                Ok(true)
            }
            VectorChange::Tensor(vectors) => {
                for vector in vectors {
                    read.control.check()?;
                    validate_vector_values(index.dimensions(), vector)?;
                }
                Ok(!vectors.is_empty())
            }
            VectorChange::Delete => Ok(false),
        }
    })()
    .map_err(|error| storage_error("validate serializable vector write", &error))?;
    if !nonempty
        && !index
            .contains_document(doc_id)
            .map_err(|error| storage_error("read original vector membership", &error))?
    {
        return Ok(());
    }
    let mut key = field_prefix(columns, field, &read.control)
        .map_err(|error| storage_error("bind serializable vector field", &error))?;
    key.extend_from_slice(&doc_id.to_be_bytes())
        .map_err(|error| storage_error("bind serializable vector candidate", &error.into()))?;
    let session = writes.serializable_session().ok_or_else(|| {
        SQLError::Internal("serializable vector writer lost its original session".into())
    })?;
    session
        .observe_serializable_write(SerializablePredicate::point(
            read.object,
            SerializableKeySpace::Vectors,
            &key,
        ))
        .map_err(|error| storage_error("observe serializable vector write", &error))
}

#[cfg(test)]
mod tests {
    use super::{field_prefix, storage_error};
    use uqa_sql::{compile, Statement};
    use uqa_storage::read_control::StorageReadControl;

    #[test]
    fn vector_field_addresses_preserve_incarnations_and_dynamic_name_boundaries() {
        let Statement::CreateTable(mut table) =
            compile("CREATE TABLE t (v VECTOR(2))").unwrap().remove(0)
        else {
            panic!("expected table declaration");
        };
        let control = StorageReadControl::with_limit(4096);
        assert!(field_prefix(&table.columns, "v", &control).is_err());
        table.columns[0].object_id = Some([1; 16]);
        let old = field_prefix(&table.columns, "v", &control).unwrap();
        table.columns[0].name = "renamed".into();
        assert_eq!(
            &*old,
            &*field_prefix(&table.columns, "renamed", &control).unwrap()
        );
        table.columns[0].object_id = Some([2; 16]);
        assert_ne!(
            &*old,
            &*field_prefix(&table.columns, "renamed", &control).unwrap()
        );
        let dynamic = field_prefix(&[], "v", &control).unwrap();
        assert_ne!(&*old, &*dynamic);
        assert_eq!(
            &*dynamic,
            &*field_prefix(&table.columns, "v", &control).unwrap()
        );
        for name in ["vv", "v\0", "v\0\0", "日本語"] {
            let other = field_prefix(&[], name, &control).unwrap();
            assert!(!other.starts_with(&dynamic));
            assert!(!dynamic.starts_with(&other));
        }
    }

    #[test]
    fn vector_field_binding_uses_the_original_allowance_and_cancellation() {
        let control = StorageReadControl::with_limit(32);
        let retained = field_prefix(&[], "vector", &control).unwrap();
        let held = control.memory().used();
        let error = field_prefix(&[], &"long".repeat(32), &control).unwrap_err();
        assert_eq!(
            storage_error("vector field", &error).sqlstate(),
            Some("53200")
        );
        assert_eq!(control.memory().used(), held);
        control.cancellation().cancel();
        let error = field_prefix(&[], "v", &control).unwrap_err();
        assert_eq!(
            storage_error("vector field", &error).sqlstate(),
            Some("57014")
        );
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}
