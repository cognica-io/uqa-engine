//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::invalid;
use super::origins::OriginVerifier;
use crate::diskann_index::format::{
    decode_codebook_from, decode_page, DiskANNManifest, DiskANNNode, DiskANNQuantizationIdentity,
    DiskANNSideLayout, PAGE_HEADER_BYTES,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

/// Complete physical streams, including the reserved cycle and adjacency fingerprint when build provenance is present. This grants no canonical-snapshot or MVCC publication authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNArtifactSeal {
    manifest: DiskANNManifest,
}

impl DiskANNArtifactSeal {
    pub fn manifest(&self) -> &DiskANNManifest {
        &self.manifest
    }
}

pub struct DiskANNArtifactSealer {
    manifest: DiskANNManifest,
    identity: Option<DiskANNQuantizationIdentity>,
    next_page: u64,
    next_code: u64,
    next_side: u64,
    last_node: Option<(u64, u32)>,
    last_side: Option<(u64, u32)>,
    graph: Sha256,
    codes: Sha256,
    side: Sha256,
    adjacency: Sha256,
    edges: u64,
    slot: BudgetedVec<u8>,
    failed: bool,
    control: StorageReadControl,
    origins: Option<OriginVerifier>,
}

impl DiskANNArtifactSealer {
    pub fn new(
        manifest: DiskANNManifest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            manifest,
            identity: None,
            next_page: 0,
            next_code: 0,
            next_side: 0,
            last_node: None,
            last_side: None,
            graph: Sha256::new(),
            codes: Sha256::new(),
            side: Sha256::new(),
            adjacency: crate::diskann_index::format::adjacency_hash(
                manifest.input().coverage,
                manifest.input().nodes,
                (manifest.input().parameters.max_degree as u64)
                    .min(manifest.input().nodes.saturating_sub(1)) as usize,
            ),
            edges: 0,
            slot: BudgetedVec::new(control.memory()),
            failed: false,
            control: control.clone(),
            origins: manifest
                .origins()
                .map(|_| OriginVerifier::new(&manifest))
                .transpose()?,
        })
    }

    fn begin(&mut self) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.failed {
            return Err(invalid("artifact sealer previously failed"));
        }
        self.failed = true;
        Ok(())
    }

    pub fn codebook(&mut self, bytes: &[u8]) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.codebook_from(bytes, &mut ()))
    }

    pub(super) async fn codebook_from(
        &mut self,
        bytes: &[u8],
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        self.begin()?;
        if self.identity.is_some() {
            return Err(invalid("repeated codebook"));
        }
        let (_, identity) =
            decode_codebook_from(&self.manifest, bytes, &self.control, work).await?;
        self.identity = Some(identity);
        self.failed = false;
        Ok(())
    }

    pub fn code_batch(&mut self, first: u64, bytes: &[u8]) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.code_batch_from(first, bytes, &mut ()))
    }

    pub(super) async fn code_batch_from(
        &mut self,
        first: u64,
        bytes: &[u8],
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        self.begin()?;
        if first != self.next_code {
            return Err(invalid("noncontiguous code batches"));
        }
        let identity = self
            .identity
            .ok_or_else(|| invalid("codebook must precede codes"))?;
        let batch = identity
            .decode_codes_from(first, bytes, &self.control, work)
            .await?;
        if let Some(build) = self.manifest.build_provenance() {
            let expected =
                (self.manifest.input().nodes - first).min(build.code_batch_nodes() as u64);
            if batch.node_count() != expected {
                return Err(invalid("code batch differs from build provenance"));
            }
        }
        for part in batch.bytes().chunks(4096) {
            work.checkpoint().await?;
            self.control.check()?;
            self.codes.update(part);
        }
        self.next_code += batch.node_count();
        self.failed = false;
        Ok(())
    }

    pub fn side_batch(&mut self, first: u64, bytes: &[u8]) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.side_batch_from(first, bytes, &mut ()))
    }

    pub(super) async fn side_batch_from(
        &mut self,
        first: u64,
        bytes: &[u8],
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        self.begin()?;
        if first != self.next_side {
            return Err(invalid("noncontiguous side batches"));
        }
        let input = self.manifest.input();
        let layout =
            DiskANNSideLayout::new(input.generation, input.dimensions, input.side_vectors)?;
        let batch = layout
            .decode_from(first, bytes, &self.control, work)
            .await?;
        if let Some(build) = self.manifest.build_provenance() {
            let expected = (input.side_vectors - first).min(build.side_batch_entries() as u64);
            if batch.record_count() as u64 != expected {
                return Err(invalid("side batch differs from build provenance"));
            }
        }
        for index in 0..batch.record_count() {
            work.checkpoint().await?;
            self.control.check()?;
            let entry = batch.entry(index).expect("validated side entry");
            let key = (entry.doc_id(), entry.ordinal());
            if self.last_side.is_some_and(|last| last >= key) {
                return Err(invalid("side entries cross batch order"));
            }
            self.last_side = Some(key);
        }
        for part in batch.bytes().chunks(4096) {
            work.checkpoint().await?;
            self.control.check()?;
            self.side.update(part);
        }
        self.next_side += batch.record_count() as u64;
        self.failed = false;
        Ok(())
    }

    pub fn graph_page(&mut self, id: u64, bytes: &[u8]) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.graph_page_from(id, bytes, &mut ()))
    }

    pub(super) async fn graph_page_from(
        &mut self,
        id: u64,
        bytes: &[u8],
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        self.begin()?;
        work.checkpoint().await?;
        if id != self.next_page {
            return Err(invalid("noncontiguous graph pages"));
        }
        let layout = self.manifest.layout();
        let page = decode_page(
            self.manifest.input().generation,
            layout,
            id,
            bytes,
            &self.control,
        )?;
        let shape = page.shape();
        if shape.fragments == 1 {
            for (slot, raw) in page.payload().chunks_exact(layout.slot_bytes()).enumerate() {
                let node = layout
                    .decode_node_from(shape.first_node + slot as u64, raw, &self.control, work)
                    .await?;
                self.node_from(&node, work).await?;
            }
        } else {
            if shape.fragment_index == 0 {
                self.slot.clear();
                self.slot.reserve(layout.slot_bytes())?;
            }
            self.slot.extend_from_slice(page.payload())?;
            if shape.fragment_index + 1 == shape.fragments {
                let node = layout
                    .decode_node_from(shape.first_node, &self.slot, &self.control, work)
                    .await?;
                self.node_from(&node, work).await?;
                self.slot.clear();
            }
        }
        self.graph
            .update(&bytes[PAGE_HEADER_BYTES - 32..PAGE_HEADER_BYTES]);
        self.next_page += 1;
        self.control.check()?;
        self.failed = false;
        Ok(())
    }

    pub fn origin_batch(&mut self, first: u64, bytes: &[u8]) -> StorageBackendResult<()> {
        crate::image_io::work::inline(self.origin_batch_from(first, bytes, &mut ()))
    }

    pub(super) async fn origin_batch_from(
        &mut self,
        first: u64,
        bytes: &[u8],
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        self.begin()?;
        self.origins
            .as_mut()
            .ok_or_else(|| invalid("unexpected origin artifact"))?
            .batch_from(first, bytes, &self.control, work)
            .await?;
        self.control.check()?;
        self.failed = false;
        Ok(())
    }

    async fn node_from(
        &mut self,
        node: &DiskANNNode,
        work: &mut (impl crate::image_io::work::Work + Send),
    ) -> StorageBackendResult<()> {
        let key = (node.doc_id(), node.ordinal());
        if self.last_node.is_some_and(|last| last >= key) {
            return Err(invalid(
                "graph logical identities are not unique and ordered",
            ));
        }
        self.last_node = Some(key);
        if self.manifest.build_provenance().is_some() {
            let input = self.manifest.input();
            let degree =
                (input.parameters.max_degree as u64).min(input.nodes.saturating_sub(1)) as usize;
            if input.nodes > 1 {
                let successor = (node.node_id() + 1) % input.nodes;
                let mut found = false;
                for neighbors in node.neighbors().chunks(1024) {
                    work.checkpoint().await?;
                    self.control.check()?;
                    if neighbors.contains(&successor) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(invalid("built graph is missing its global successor"));
                }
            }
            self.edges = self
                .edges
                .checked_add(node.neighbors().len() as u64)
                .ok_or_else(|| invalid("built graph edge overflow"))?;
            self.adjacency
                .update((node.neighbors().len() as u64).to_le_bytes());
            for slot in 0..degree {
                if slot.is_multiple_of(1024) {
                    work.checkpoint().await?;
                }
                self.control.check()?;
                self.adjacency.update(
                    node.neighbors()
                        .get(slot)
                        .copied()
                        .unwrap_or(0)
                        .to_le_bytes(),
                );
            }
        }
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<DiskANNArtifactSeal> {
        self.control.check()?;
        if let Some(origins) = self.origins {
            origins.finish()?;
        }
        let input = self.manifest.input();
        if self.failed
            || self.next_page != self.manifest.layout().page_count()
            || self.next_code != input.nodes
            || self.next_side != input.side_vectors
            || self.identity.is_some() != (input.nodes != 0)
            || !self.slot.is_empty()
        {
            return Err(invalid(
                "artifact streams are incomplete or previously failed",
            ));
        }
        for (hash, expected) in [
            (self.graph, input.artifacts.graph),
            (self.codes, input.artifacts.codes),
            (self.side, input.artifacts.side),
        ] {
            if <[u8; 32]>::from(hash.finalize()) != expected {
                return Err(invalid("artifact stream digest differs from manifest"));
            }
        }
        if let Some(build) = self.manifest.build_provenance() {
            if self.edges != build.edges()
                || <[u8; 32]>::from(self.adjacency.finalize()) != build.adjacency_digest()
            {
                return Err(invalid(
                    "built graph differs from its construction provenance",
                ));
            }
        }
        Ok(DiskANNArtifactSeal {
            manifest: self.manifest,
        })
    }
}
