//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

use super::{
    cache::{PageCache, SharedPage},
    check_catalog, invalid, read_record, DiskANNPageSource, DiskANNReadCapabilities,
    DiskANNReadLimits, DiskANNRecordKey,
};
use crate::diskann_index::{
    format::{
        decode_codebook, decode_page, DiskANNManifest, DiskANNNode, DiskANNQuantizationIdentity,
        DiskANNSideEntry, DiskANNSideLayout, PAGE_BYTES,
    },
    PQCodebook,
};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};

enum PageBytes {
    Owned(BudgetedVec<u8>),
    Cached(SharedPage),
}

pub struct DiskANNPageLease {
    id: u64,
    bytes: PageBytes,
}

impl DiskANNPageLease {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn bytes(&self) -> &[u8] {
        match &self.bytes {
            PageBytes::Owned(bytes) => bytes,
            PageBytes::Cached(bytes) => bytes,
        }
    }
}

struct Resident {
    source: Arc<dyn DiskANNPageSource>,
    manifest: DiskANNManifest,
    codebook: Option<PQCodebook>,
    codes: BudgetedVec<u8>,
    cache: PageCache,
    limits: DiskANNReadLimits,
}

#[derive(Clone)]
pub struct DiskANNReader {
    resident: Arc<Budgeted<Resident>>,
}

impl DiskANNReader {
    pub fn open(
        source: Arc<dyn DiskANNPageSource>,
        dimensions: u32,
        parameters: DiskANNIndexParams,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        parameters.validate(dimensions)?;
        let bytes = read_record(
            &*source,
            DiskANNRecordKey::Manifest,
            limits.max_record_bytes,
            control,
        )?;
        let manifest = DiskANNManifest::decode(source.generation(), &bytes, control)?;
        check_catalog(&manifest, dimensions, parameters)?;
        drop(bytes);
        if manifest.input().nodes != 0 && limits.max_in_flight_page_bytes < PAGE_BYTES {
            return Err(MemoryError::Limit {
                required: PAGE_BYTES,
                limit: limits.max_in_flight_page_bytes,
            }
            .into());
        }
        let budget = control.memory().child(limits.resident_bytes);
        let resident_control = StorageReadControl::new(&budget, control.cancellation());
        let mut codes = BudgetedVec::new(&budget);
        let codebook = if manifest.input().nodes == 0 {
            None
        } else {
            let count = usize::try_from(manifest.input().nodes)
                .ok()
                .and_then(|nodes| nodes.checked_mul(parameters.pq_bytes))
                .ok_or(MemoryError::SizeOverflow)?;
            codes.reserve(count)?;
            let bytes = read_record(
                &*source,
                DiskANNRecordKey::Codebook,
                limits.max_record_bytes,
                control,
            )?;
            let (book, identity) = decode_codebook(&manifest, &bytes, &resident_control)?;
            drop(bytes);
            load_codes(
                &*source,
                identity,
                &mut codes,
                limits.max_record_bytes,
                control,
            )?;
            if crate::diskann_index::format::artifact_digest(&codes, control)?
                != manifest.input().artifacts.codes
            {
                return Err(invalid("resident code stream digest differs"));
            }
            Some(book)
        };
        let resident = Resident {
            source,
            manifest,
            codebook,
            codes,
            cache: PageCache::new(control.memory(), limits.cache_bytes),
            limits,
        };
        let resident = Budgeted::new(resident, budget.empty_reservation()).into_shared()?;
        control.check()?;
        Ok(Self { resident })
    }

    pub fn manifest(&self) -> &DiskANNManifest {
        &self.resident.manifest
    }
    pub fn codebook(&self) -> Option<&PQCodebook> {
        self.resident.codebook.as_ref()
    }
    pub fn capabilities(&self) -> DiskANNReadCapabilities {
        self.resident.source.capabilities()
    }
    pub fn cache_bytes(&self) -> usize {
        self.resident.cache.used()
    }
    pub fn code(&self, node: u64) -> Option<&[u8]> {
        if node >= self.manifest().input().nodes {
            return None;
        }
        let width = self.manifest().input().parameters.pq_bytes;
        let start = node as usize * width;
        Some(&self.resident.codes[start..start + width])
    }

    /// IDs must be strictly increasing. Returned leases preserve that order regardless of provider completion order.
    pub fn read_pages(
        &self,
        ids: &[u64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DiskANNPageLease>> {
        control.check()?;
        if self.resident.source.generation() != self.manifest().input().generation {
            return Err(invalid("source changed its retained generation"));
        }
        let mut previous = None;
        for &id in ids {
            control.check()?;
            self.manifest().layout().page_shape(id)?;
            if previous.is_some_and(|last| last >= id) {
                return Err(invalid("page requests must be unique and increasing"));
            }
            previous = Some(id);
        }
        let mut result = BudgetedVec::new(control.memory());
        result.reserve(ids.len())?;
        let batch = self
            .capabilities()
            .max_batch_pages()
            .min(self.resident.limits.max_in_flight_page_bytes / PAGE_BYTES);
        if !ids.is_empty() && batch == 0 {
            return Err(invalid("no page fits the read limit"));
        }
        for ids in ids.chunks(batch.max(1)) {
            self.read_batch(ids, &mut result, control)?;
        }
        control.check()?;
        Ok(result)
    }

    fn read_batch(
        &self,
        ids: &[u64],
        result: &mut BudgetedVec<DiskANNPageLease>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        struct Slot {
            id: u64,
            cached: Option<SharedPage>,
            bytes: BudgetedVec<u8>,
            complete: bool,
        }
        let mut slots = BudgetedVec::new(control.memory());
        let mut missing = BudgetedVec::new(control.memory());
        slots.reserve(ids.len())?;
        missing.reserve(ids.len())?;
        for &id in ids {
            control.check()?;
            let cached = self.resident.cache.get(id);
            let mut bytes = BudgetedVec::new(control.memory());
            if cached.is_none() {
                bytes.reserve(PAGE_BYTES)?;
                for _ in 0..PAGE_BYTES {
                    bytes.push(0)?;
                }
                missing.push(id)?;
            }
            slots.push(Slot {
                id,
                cached,
                bytes,
                complete: false,
            })?;
        }
        if !missing.is_empty() {
            let mut failure = None;
            let source_result =
                self.resident
                    .source
                    .read_graph_pages(&missing, control, &mut |id, bytes| {
                        if failure.is_some() {
                            return Err(invalid("page visitor already failed"));
                        }
                        let mut accept = || -> StorageBackendResult<()> {
                            control.check()?;
                            let index = ids
                                .binary_search(&id)
                                .map_err(|_| invalid("unrequested page completion"))?;
                            let slot = &mut slots[index];
                            if slot.complete || slot.cached.is_some() {
                                return Err(invalid(
                                    "duplicate or unrequested cached page completion",
                                ));
                            }
                            decode_page(
                                self.manifest().input().generation,
                                self.manifest().layout(),
                                id,
                                bytes,
                                control,
                            )?;
                            slot.bytes.copy_from_slice(bytes);
                            slot.complete = true;
                            Ok(())
                        };
                        if let Err(error) = accept() {
                            failure = Some(error);
                            return Err(invalid("page visitor rejected data"));
                        }
                        Ok(())
                    });
            if let Some(error) = failure {
                return Err(error);
            }
            source_result?;
        }
        if slots
            .iter()
            .any(|slot| slot.cached.is_none() && !slot.complete)
        {
            return Err(invalid("missing page completion"));
        }
        let (slots, _slots_memory) = slots.into_parts();
        for slot in slots {
            let bytes = match slot.cached {
                Some(page) => PageBytes::Cached(page),
                None => match self.resident.cache.admit(slot.id, &slot.bytes, control)? {
                    Some(page) => PageBytes::Cached(page),
                    None => PageBytes::Owned(slot.bytes),
                },
            };
            result.push(DiskANNPageLease { id: slot.id, bytes })?;
        }
        Ok(())
    }

    pub fn read_node(
        &self,
        node: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNNode> {
        control.check()?;
        let layout = self.manifest().layout();
        let address = layout.node_address(node)?;
        if address.fragments == 1 {
            let pages = self.read_pages(&[address.first_page], control)?;
            let page = decode_page(
                self.manifest().input().generation,
                layout,
                address.first_page,
                pages[0].bytes(),
                control,
            )?;
            let start = address.slot as usize * layout.slot_bytes();
            return layout.decode_node(
                node,
                &page.payload()[start..start + layout.slot_bytes()],
                control,
            );
        }
        let mut slot = BudgetedVec::new(control.memory());
        slot.reserve(layout.slot_bytes())?;
        for offset in 0..address.fragments {
            let id = address.first_page + u64::from(offset);
            let pages = self.read_pages(&[id], control)?;
            let page = decode_page(
                self.manifest().input().generation,
                layout,
                id,
                pages[0].bytes(),
                control,
            )?;
            slot.extend_from_slice(page.payload())?;
        }
        layout.decode_node(node, &slot, control)
    }

    /// Scan numeric-side references for an internal consumer that applies canonical snapshot visibility and scoring. Completion verifies the whole stream digest; on any error the consumer must discard its partial state without exposing rows or publishing effects.
    pub fn visit_side(
        &self,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DiskANNSideEntry) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let input = self.manifest().input();
        let layout =
            DiskANNSideLayout::new(input.generation, input.dimensions, input.side_vectors)?;
        let mut next = 0;
        let mut previous = None;
        let mut digest = Sha256::new();
        while next < input.side_vectors {
            let bytes = read_record(
                &*self.resident.source,
                DiskANNRecordKey::Side(next),
                self.resident.limits.max_record_bytes,
                control,
            )?;
            let batch = layout.decode(next, &bytes, control)?;
            for part in batch.bytes().chunks(4096) {
                control.check()?;
                digest.update(part);
            }
            for index in 0..batch.record_count() {
                control.check()?;
                let entry = batch.entry(index).expect("validated side batch");
                let key = (entry.doc_id(), entry.ordinal());
                if previous.is_some_and(|last| last >= key) {
                    return Err(invalid("side order crosses a batch boundary"));
                }
                previous = Some(key);
                visit(entry)?;
            }
            next += batch.record_count() as u64;
        }
        if <[u8; 32]>::from(digest.finalize()) != input.artifacts.side {
            return Err(invalid("side stream digest differs"));
        }
        control.check()
    }
}

fn load_codes(
    source: &dyn DiskANNPageSource,
    identity: DiskANNQuantizationIdentity,
    result: &mut BudgetedVec<u8>,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut next = 0;
    while next < identity.node_count() {
        let bytes = read_record(source, DiskANNRecordKey::Codes(next), limit, control)?;
        let batch = identity.decode_codes(next, &bytes, control)?;
        for part in batch.bytes().chunks(4096) {
            control.check()?;
            result.extend_from_slice(part)?;
        }
        next += batch.node_count();
    }
    control.check()
}
