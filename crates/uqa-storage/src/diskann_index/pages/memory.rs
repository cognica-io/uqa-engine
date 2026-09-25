//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget};

use super::{
    copy, invalid, DiskANNArtifactSeal, DiskANNArtifactSealer, DiskANNPageSource,
    DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordKey, DiskANNRecordVisitor,
};
use crate::diskann_index::format::{DiskANNGeneration, DiskANNManifest, PAGE_BYTES};
use crate::{read_control::StorageReadControl, StorageBackendResult};

struct Records {
    records: BudgetedMap<DiskANNRecordKey, BudgetedVec<u8>>,
    graph: BudgetedMap<u64, BudgetedVec<u8>>,
}

pub struct DiskANNMemoryBuilder {
    generation: DiskANNGeneration,
    records: Records,
    budget: MemoryBudget,
}

#[derive(Clone)]
pub struct DiskANNMemorySource {
    seal: DiskANNArtifactSeal,
    records: Arc<Budgeted<Records>>,
}

impl DiskANNMemoryBuilder {
    pub fn new(generation: DiskANNGeneration, budget: &MemoryBudget) -> Self {
        Self {
            generation,
            records: Records {
                records: BudgetedMap::new(budget),
                graph: BudgetedMap::new(budget),
            },
            budget: budget.clone(),
        }
    }

    pub fn write_record(
        &mut self,
        key: DiskANNRecordKey,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if key == DiskANNRecordKey::Manifest || self.records.records.contains_key(&key) {
            return Err(invalid(
                "manifest is written at sealing; staging records cannot be replaced",
            ));
        }
        let owner = StorageReadControl::new(&self.budget, control.cancellation());
        let bytes = copy(bytes, usize::MAX, &owner)?;
        self.records.records.insert(key, bytes)?;
        Ok(())
    }

    pub fn write_graph_page(
        &mut self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if bytes.len() != PAGE_BYTES || self.records.graph.contains_key(&id) {
            return Err(invalid("staged page length or identity differs"));
        }
        let owner = StorageReadControl::new(&self.budget, control.cancellation());
        let bytes = copy(bytes, PAGE_BYTES, &owner)?;
        self.records.graph.insert(id, bytes)?;
        Ok(())
    }

    pub fn finish(
        mut self,
        manifest: DiskANNManifest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNMemorySource> {
        control.check()?;
        if manifest.input().generation != self.generation {
            return Err(invalid("manifest does not belong to staging owner"));
        }
        let mut sealer = DiskANNArtifactSealer::new(manifest, control)?;
        for (key, bytes) in &self.records.records {
            match key {
                DiskANNRecordKey::Codebook => sealer.codebook(bytes)?,
                DiskANNRecordKey::Codes(first) => sealer.code_batch(*first, bytes)?,
                DiskANNRecordKey::Side(first) => sealer.side_batch(*first, bytes)?,
                DiskANNRecordKey::Manifest => {
                    return Err(invalid("staged manifest before sealing"))
                }
            }
        }
        for (id, bytes) in &self.records.graph {
            sealer.graph_page(*id, bytes)?;
        }
        let seal = sealer.finish()?;
        let owner = StorageReadControl::new(&self.budget, control.cancellation());
        self.records
            .records
            .insert(DiskANNRecordKey::Manifest, manifest.encode(&owner)?)?;
        let records = Budgeted::new(self.records, self.budget.empty_reservation()).into_shared()?;
        control.check()?;
        Ok(DiskANNMemorySource { seal, records })
    }
}

impl DiskANNPageSource for DiskANNMemorySource {
    fn generation(&self) -> DiskANNGeneration {
        self.seal.manifest().input().generation
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        DiskANNReadCapabilities {
            max_batch_pages: 32,
            read_concurrency: 1,
        }
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let bytes = self
            .records
            .records
            .get(&key)
            .ok_or_else(|| invalid("missing memory record"))?;
        if bytes.len() > max_bytes {
            return Err(uqa_core::memory::MemoryError::Limit {
                required: bytes.len(),
                limit: max_bytes,
            }
            .into());
        }
        visit(bytes)?;
        control.check()
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if pages.len() > self.capabilities().max_batch_pages() {
            return Err(invalid("page request exceeds source batch limit"));
        }
        for &id in pages {
            control.check()?;
            let bytes = self
                .records
                .graph
                .get(&id)
                .ok_or_else(|| invalid("missing memory page"))?;
            visit(id, bytes)?;
        }
        control.check()
    }
}
