//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    sync::Arc,
};
use uqa_core::DocId;
use uqa_storage::{
    diskann_index::{
        format::{
            DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNGeneration, DiskANNVectorVersion,
            PAGE_BYTES,
        },
        pages::DiskANNReadLimits,
        DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
        RetainedDiskANNIndex,
    },
    key_value::conformance::build_diskann_memory_fixture,
    mvcc::{DatabaseId, StorageTransactionId},
    read_control::StorageReadControl,
    vector_index::DiskANNIndexParams,
    MemoryVectorIndex, StorageBackendResult, VectorIndex,
};

#[derive(Clone)]
struct Canonical {
    documents: Arc<BTreeMap<DocId, Vec<Vec<f32>>>>,
    control: StorageReadControl,
}

fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([93; 16]), 1).unwrap(),
        1,
    )
    .unwrap()
}

impl DiskANNCanonicalRead for Canonical {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        Ok(self
            .documents
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .next()
            .map(|(&doc, _)| doc))
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        Ok(self.documents.contains_key(&document).then(version))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        let Some(values) = self.documents.get(&document) else {
            return Ok(None);
        };
        for (ordinal, raw) in values.iter().enumerate() {
            visit(ordinal as u32, version(), raw)?;
        }
        self.check_control(control)?;
        Ok(Some(version()))
    }
}

impl DiskANNQueryRead for Canonical {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check_control(control)?;
        self.documents
            .get(&document)
            .map(|values| DiskANNCanonicalOrigin::new(version(), 2, values.len() as u64))
            .transpose()
    }
    fn next_change_after(
        &self,
        _: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.check_control(control)?;
        Ok(None)
    }
}

pub(super) fn indexes(
    control: &StorageReadControl,
) -> (Arc<dyn VectorIndex>, Arc<dyn VectorIndex>) {
    let source = Canonical {
        documents: Arc::new(BTreeMap::from([
            (1, vec![vec![0.0, 1.0], vec![1.0, 0.0]]),
            (2, vec![vec![0.0, 1.0]]),
            (3, vec![]),
            (4, vec![vec![-1.0, 0.0]]),
        ])),
        control: control.clone(),
    };
    let mut exact = MemoryVectorIndex::new(2);
    for (&document, values) in &*source.documents {
        exact.add_many(document, values.clone()).unwrap();
    }
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 1,
        beam_width: 1,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).unwrap()
    };
    let physical = build_diskann_memory_fixture(
        DiskANNGeneration::new([94; 16], 1, 2, 3).unwrap(),
        source.clone(),
        parameters,
        control,
    )
    .unwrap();
    let index = RetainedDiskANNIndex::open(
        source,
        Arc::new(physical),
        parameters,
        DiskANNReadLimits {
            resident_bytes: 65_536,
            cache_bytes: PAGE_BYTES,
            max_in_flight_page_bytes: 2 * PAGE_BYTES,
            max_record_bytes: 8192,
        },
        control,
    )
    .unwrap();
    (index.snapshot().unwrap(), exact.snapshot().unwrap())
}
