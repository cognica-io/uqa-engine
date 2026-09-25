//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, NODE_HEADER_BYTES, PAGE_PAYLOAD_BYTES};
use crate::StorageBackendResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNNodeLayout {
    pub(super) dimensions: u32,
    pub(super) max_degree: usize,
    pub(super) node_count: u64,
    pub(super) slot_bytes: usize,
    nodes_per_page: u32,
    fragments: u32,
    page_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNNodeAddress {
    pub first_page: u64,
    pub slot: u32,
    pub fragments: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNPageShape {
    pub first_node: u64,
    pub slots: u32,
    pub fragment_index: u32,
    pub fragments: u32,
    pub payload_bytes: u32,
}

impl DiskANNNodeLayout {
    pub fn new(dimensions: u32, max_degree: usize, node_count: u64) -> StorageBackendResult<Self> {
        if dimensions == 0 || max_degree < 2 {
            return Err(invalid(
                "require positive dimensions and maximum degree at least two",
            ));
        }
        let dimensions_usize =
            usize::try_from(dimensions).map_err(|_| invalid("dimension range"))?;
        let slot_bytes = max_degree
            .checked_mul(8)
            .and_then(|neighbors| {
                dimensions_usize
                    .checked_mul(4)
                    .and_then(|vectors| vectors.checked_add(neighbors))
            })
            .and_then(|body| body.checked_add(NODE_HEADER_BYTES))
            .ok_or_else(|| invalid("node slot size overflow"))?;
        std::alloc::Layout::array::<u8>(slot_bytes)
            .map_err(|_| invalid("node slot exceeds addressable memory"))?;
        let fragments = u32::try_from(slot_bytes.div_ceil(PAGE_PAYLOAD_BYTES))
            .map_err(|_| invalid("fragment count exceeds format"))?;
        let nodes_per_page = (PAGE_PAYLOAD_BYTES / slot_bytes) as u32;
        let page_count = if nodes_per_page == 0 {
            node_count
                .checked_mul(u64::from(fragments))
                .ok_or_else(|| invalid("page count overflow"))?
        } else {
            node_count.div_ceil(u64::from(nodes_per_page))
        };
        Ok(Self {
            dimensions,
            max_degree,
            node_count,
            slot_bytes,
            nodes_per_page,
            fragments,
            page_count,
        })
    }

    pub fn dimensions(self) -> u32 {
        self.dimensions
    }
    pub fn max_degree(self) -> usize {
        self.max_degree
    }
    pub fn node_count(self) -> u64 {
        self.node_count
    }
    pub fn slot_bytes(self) -> usize {
        self.slot_bytes
    }
    pub fn page_count(self) -> u64 {
        self.page_count
    }

    pub fn node_address(self, node: u64) -> StorageBackendResult<DiskANNNodeAddress> {
        if node >= self.node_count {
            return Err(invalid("node ID outside generation"));
        }
        if self.nodes_per_page > 0 {
            let count = u64::from(self.nodes_per_page);
            Ok(DiskANNNodeAddress {
                first_page: node / count,
                slot: (node % count) as u32,
                fragments: 1,
            })
        } else {
            Ok(DiskANNNodeAddress {
                first_page: node * u64::from(self.fragments),
                slot: 0,
                fragments: self.fragments,
            })
        }
    }

    pub fn page_shape(self, page: u64) -> StorageBackendResult<DiskANNPageShape> {
        if page >= self.page_count {
            return Err(invalid("page ID outside generation"));
        }
        if self.nodes_per_page > 0 {
            let first_node = page * u64::from(self.nodes_per_page);
            let slots = (self.node_count - first_node).min(u64::from(self.nodes_per_page)) as u32;
            Ok(DiskANNPageShape {
                first_node,
                slots,
                fragment_index: 0,
                fragments: 1,
                payload_bytes: (slots as usize * self.slot_bytes) as u32,
            })
        } else {
            let fragments = u64::from(self.fragments);
            let fragment_index = (page % fragments) as u32;
            let offset = fragment_index as usize * PAGE_PAYLOAD_BYTES;
            Ok(DiskANNPageShape {
                first_node: page / fragments,
                slots: 1,
                fragment_index,
                fragments: self.fragments,
                payload_bytes: (self.slot_bytes - offset).min(PAGE_PAYLOAD_BYTES) as u32,
            })
        }
    }
}
