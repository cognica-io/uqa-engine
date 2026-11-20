//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retire the selected immutable roots without traversing subtrees that another reader owns.

use super::{Budgeted, BudgetedSharedMap, MemoryBudget, MemoryError, SharedNode};
use crate::memory::BudgetedSmallVec;
use std::sync::Arc;

pub(in crate::memory) struct SharedMapRetirement<K, V> {
    nodes: BudgetedSmallVec<[SharedNode<K, V>; 2]>,
}

impl<K, V> SharedMapRetirement<K, V> {
    pub(in crate::memory) fn prepare<const N: usize>(
        maps: [&BudgetedSharedMap<K, V>; N],
        memory: &MemoryBudget,
    ) -> Result<Self, MemoryError> {
        let roots = maps.into_iter().filter_map(|map| map.root.as_ref());
        let height = roots
            .clone()
            .map(|node| usize::from(node.height))
            .max()
            .unwrap_or(0);
        // One child continues the current path. At most one sibling per level and
        // the other roots can await a later step.
        let capacity = height
            .checked_add(roots.clone().count().saturating_sub(1))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut nodes = BudgetedSmallVec::new(memory);
        nodes.reserve(capacity)?;
        for root in roots {
            nodes.push(Arc::clone(root))?;
        }
        Ok(Self { nodes })
    }

    pub(in crate::memory) fn advance(&mut self, release: impl FnOnce(Budgeted<(K, V)>)) -> bool {
        let Some(link) = self.nodes.pop() else {
            return true;
        };
        if let Some(node) = Arc::into_inner(link) {
            let (node, _node_memory) = node.into_parts();
            for child in [node.right, node.left].into_iter().flatten() {
                self.nodes.push(child).expect("admitted shared map height");
            }
            if let Some(entry) = Arc::into_inner(node.entry) {
                release(entry);
            }
        }
        self.nodes.is_empty()
    }
}
