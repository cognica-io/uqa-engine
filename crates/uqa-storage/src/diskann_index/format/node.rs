//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::{memory::BudgetedVec, DocId};

use super::{field, invalid, zeros, DiskANNNodeLayout, DiskANNVectorVersion, NODE_HEADER_BYTES};
use crate::diskann_index::metric::{checkpoint, norms};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub struct DiskANNNodeInput<'a> {
    pub node_id: u64,
    pub doc_id: DocId,
    pub ordinal: u32,
    pub version: DiskANNVectorVersion,
    pub vector: &'a [f32],
    pub neighbors: &'a [u64],
}

#[derive(Debug)]
pub struct DiskANNNode {
    node_id: u64,
    doc_id: DocId,
    ordinal: u32,
    version: DiskANNVectorVersion,
    raw_norm: f32,
    vector: BudgetedVec<f32>,
    neighbors: BudgetedVec<u64>,
}

impl DiskANNNode {
    pub fn node_id(&self) -> u64 {
        self.node_id
    }
    pub fn doc_id(&self) -> DocId {
        self.doc_id
    }
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
    pub fn version(&self) -> DiskANNVectorVersion {
        self.version
    }
    pub fn raw_norm(&self) -> f32 {
        self.raw_norm
    }
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }
    pub fn neighbors(&self) -> &[u64] {
        &self.neighbors
    }
}

impl DiskANNNodeLayout {
    pub fn encode_node(
        self,
        node: &DiskANNNodeInput<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        control.check()?;
        self.node_address(node.node_id)?;
        self.check_neighbors(node.node_id, node.neighbors, control)?;
        let raw_norm = graph_norm(self.dimensions, node.vector, control)?;
        let mut header = [0_u8; NODE_HEADER_BYTES];
        header[0..8].copy_from_slice(&node.node_id.to_le_bytes());
        header[8..16].copy_from_slice(&node.doc_id.to_le_bytes());
        header[16..20].copy_from_slice(&node.ordinal.to_le_bytes());
        header[20..24].copy_from_slice(&raw_norm.to_bits().to_le_bytes());
        header[24..32].copy_from_slice(&(node.neighbors.len() as u64).to_le_bytes());
        header[32..48].copy_from_slice(&node.version.writer.database().as_bytes());
        header[48..56].copy_from_slice(&node.version.writer.allocation().to_le_bytes());
        header[56..64].copy_from_slice(&node.version.revision.to_le_bytes());
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.reserve(self.slot_bytes)?;
        bytes.extend_from_slice(&header)?;
        for (offset, value) in node.vector.iter().enumerate() {
            checkpoint(offset, control)?;
            bytes.extend_from_slice(&value.to_bits().to_le_bytes())?;
        }
        for slot in 0..self.max_degree {
            checkpoint(slot, control)?;
            bytes
                .extend_from_slice(&node.neighbors.get(slot).copied().unwrap_or(0).to_le_bytes())?;
        }
        control.check()?;
        Ok(bytes)
    }

    pub fn decode_node(
        self,
        expected_node: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNNode> {
        control.check()?;
        self.node_address(expected_node)?;
        if bytes.len() != self.slot_bytes || u64::from_le_bytes(field(bytes, 0)?) != expected_node {
            return Err(invalid("node slot length or identity differs"));
        }
        let degree = usize::try_from(u64::from_le_bytes(field(bytes, 24)?))
            .map_err(|_| invalid("degree range"))?;
        if degree > self.max_degree || degree as u64 >= self.node_count {
            return Err(invalid("degree exceeds generation bounds"));
        }
        let writer = StorageTransactionId::new(
            DatabaseId::from_bytes(field(bytes, 32)?),
            u64::from_le_bytes(field(bytes, 48)?),
        )
        .map_err(|_| invalid("invalid origin writer allocation"))?;
        let version = DiskANNVectorVersion::new(writer, u64::from_le_bytes(field(bytes, 56)?))?;
        let vector_end = NODE_HEADER_BYTES + self.dimensions as usize * 4;
        let mut vector = BudgetedVec::new(control.memory());
        vector.reserve(self.dimensions as usize)?;
        for (offset, component) in bytes[NODE_HEADER_BYTES..vector_end]
            .chunks_exact(4)
            .enumerate()
        {
            checkpoint(offset, control)?;
            vector.push(f32::from_bits(u32::from_le_bytes(field(component, 0)?)))?;
        }
        let raw_norm = graph_norm(self.dimensions, &vector, control)?;
        if raw_norm.to_bits() != u32::from_le_bytes(field(bytes, 20)?) {
            return Err(invalid("stored norm differs from canonical raw norm"));
        }
        let neighbor_end = vector_end + degree * 8;
        zeros(&bytes[neighbor_end..], control)?;
        let mut neighbors = BudgetedVec::new(control.memory());
        neighbors.reserve(degree)?;
        for (offset, neighbor) in bytes[vector_end..neighbor_end].chunks_exact(8).enumerate() {
            checkpoint(offset, control)?;
            neighbors.push(u64::from_le_bytes(field(neighbor, 0)?))?;
        }
        self.check_neighbors(expected_node, &neighbors, control)?;
        control.check()?;
        Ok(DiskANNNode {
            node_id: expected_node,
            doc_id: u64::from_le_bytes(field(bytes, 8)?),
            ordinal: u32::from_le_bytes(field(bytes, 16)?),
            version,
            raw_norm,
            vector,
            neighbors,
        })
    }

    fn check_neighbors(
        self,
        node: u64,
        neighbors: &[u64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if neighbors.len() > self.max_degree || neighbors.len() as u64 >= self.node_count {
            return Err(invalid("degree exceeds generation bounds"));
        }
        let mut previous = None;
        for (offset, &neighbor) in neighbors.iter().enumerate() {
            checkpoint(offset, control)?;
            if neighbor >= self.node_count
                || neighbor == node
                || previous.is_some_and(|id| id >= neighbor)
            {
                return Err(invalid(
                    "neighbors must be sorted, unique, in range and exclude self",
                ));
            }
            previous = Some(neighbor);
        }
        Ok(())
    }
}

fn graph_norm(
    dimensions: u32,
    raw: &[f32],
    control: &StorageReadControl,
) -> StorageBackendResult<f32> {
    let (norm, _) = norms(dimensions, raw, control)?;
    if !norm.is_finite() || norm == 0.0 {
        return Err(invalid(
            "non-navigable vector belongs in the exact side stream",
        ));
    }
    Ok(norm)
}
