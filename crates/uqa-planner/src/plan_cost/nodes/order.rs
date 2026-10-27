//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep a producer attached to its defining query even when its first reference is encountered inside another subtree.

use super::PlanCostNode;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

pub(super) fn preorder<'p>(
    mut nodes: Produced<Vec<PlanCostNode<'p>>>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<PlanCostNode<'p>>>, SQLError> {
    if already_preordered(&nodes, control)? {
        return Ok(nodes);
    }
    let mut positions = ProductionVec::new(control);
    let mut next_child = ProductionVec::new(control);
    for _ in nodes.iter() {
        positions.push_copy(1usize)?;
        next_child.push_copy(0usize)?;
    }
    // Parents precede children, so the reverse pass computes complete subtree sizes. Every sum is bounded by the existing node count.
    for (index, node) in nodes.iter().enumerate().rev() {
        control.check()?;
        if let Some(parent) = node.parent {
            let size = positions[index];
            positions.as_mut_slice()[parent] += size;
        }
    }
    let mut next_root = 0;
    for (index, node) in nodes.as_mut_slice().iter_mut().enumerate() {
        control.check()?;
        let size = positions[index];
        let position = if let Some(parent) = node.parent {
            let position = next_child[parent];
            next_child.as_mut_slice()[parent] += size;
            node.parent = Some(positions[parent]);
            position
        } else {
            let position = next_root;
            next_root += size;
            position
        };
        // A node's subtree size is consumed once; its slot then holds the final index used by its children.
        positions.as_mut_slice()[index] = position;
        next_child.as_mut_slice()[index] = position + 1;
    }
    // Move each copy-only record directly into its final slot, retaining the original admitted output buffer.
    for index in 0..nodes.len() {
        while positions[index] != index {
            control.check()?;
            let destination = positions[index];
            nodes.as_mut_slice().swap(index, destination);
            positions.as_mut_slice().swap(index, destination);
        }
    }
    control.check()?;
    Ok(nodes)
}

fn already_preordered(
    nodes: &[PlanCostNode<'_>],
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut ancestor = None;
    for (index, node) in nodes.iter().enumerate() {
        control.check()?;
        debug_assert!(node.parent.is_none_or(|parent| parent < index));
        while ancestor != node.parent {
            control.check()?;
            let Some(parent) = ancestor else {
                return Ok(false);
            };
            ancestor = nodes[parent].parent;
        }
        ancestor = Some(index);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
