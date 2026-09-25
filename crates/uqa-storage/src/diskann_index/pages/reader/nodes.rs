//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::{invalid, DiskANNPageLease, DiskANNReader};
use crate::diskann_index::format::{decode_page, DiskANNNode, DiskANNPage};
use crate::{read_control::StorageReadControl, StorageBackendResult};

impl DiskANNReader {
    /// Decode unique nodes in the supplied priority order. Shared pages are read once per call, even with no cache; fragment requests obey the provider's page batch limit. All page leases and decoded nodes share the caller's allowance.
    pub fn read_nodes(
        &self,
        ids: &[u64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DiskANNNode>> {
        control.check()?;
        let layout = self.manifest().layout();
        let mut ordered = BudgetedVec::new(control.memory());
        ordered.extend_from_slice(ids)?;
        ordered.sort_unstable();
        let mut pages = BudgetedVec::new(control.memory());
        let mut previous = None;
        for &node in ordered.iter() {
            control.check()?;
            if previous == Some(node) {
                return Err(invalid("node requests must be unique"));
            }
            let address = layout.node_address(node)?;
            for fragment in 0..address.fragments {
                control.check()?;
                let id = address.first_page + u64::from(fragment);
                if pages.last() != Some(&id) {
                    pages.push(id)?;
                }
            }
            previous = Some(node);
        }
        drop(ordered);
        let leases = self.read_pages(&pages, control)?;
        drop(pages);
        let mut result = BudgetedVec::new(control.memory());
        result.reserve(ids.len())?;
        let mut slot = BudgetedVec::new(control.memory());
        for &node in ids {
            control.check()?;
            let address = layout.node_address(node)?;
            let first = leases
                .binary_search_by_key(&address.first_page, DiskANNPageLease::id)
                .map_err(|_| invalid("missing node page lease"))?;
            let decoded = if address.fragments == 1 {
                let page = self.decode_lease(&leases[first], control)?;
                let start = address.slot as usize * layout.slot_bytes();
                layout.decode_node(
                    node,
                    &page.payload()[start..start + layout.slot_bytes()],
                    control,
                )?
            } else {
                slot.clear();
                slot.reserve(layout.slot_bytes())?;
                for lease in &leases[first..first + address.fragments as usize] {
                    slot.extend_from_slice(self.decode_lease(lease, control)?.payload())?;
                }
                layout.decode_node(node, &slot, control)?
            };
            result.push(decoded)?;
        }
        control.check()?;
        Ok(result)
    }

    fn decode_lease<'a>(
        &self,
        lease: &'a DiskANNPageLease,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPage<'a>> {
        decode_page(
            self.manifest().input().generation,
            self.manifest().layout(),
            lease.id(),
            lease.bytes(),
            control,
        )
    }
}
