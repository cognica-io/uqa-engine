//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy only the changed AVL path; rotations share immutable entries and off-path trees.

use std::cmp::Ordering;

use super::{Budgeted, Entry, Link, MemoryBudget, MemoryError, Node, SharedNode};

pub(super) fn insert<K: Ord, V>(
    link: &Link<K, V>,
    entry: Entry<K, V>,
    memory: &MemoryBudget,
) -> Result<(SharedNode<K, V>, bool), MemoryError> {
    let Some(node) = link else {
        return Ok((make_node(entry, None, None, memory)?, true));
    };
    let (left, right, added) = match entry.0.cmp(&node.entry.0) {
        Ordering::Less => {
            let (left, added) = insert(&node.left, entry, memory)?;
            (Some(left), node.right.clone(), added)
        }
        Ordering::Greater => {
            let (right, added) = insert(&node.right, entry, memory)?;
            (node.left.clone(), Some(right), added)
        }
        Ordering::Equal => {
            return Ok((
                make_node(entry, node.left.clone(), node.right.clone(), memory)?,
                false,
            ));
        }
    };
    Ok((balance(node.entry.clone(), left, right, memory)?, added))
}

fn height<K, V>(link: &Link<K, V>) -> u8 {
    link.as_ref().map_or(0, |node| node.height)
}

fn make_node<K, V>(
    entry: Entry<K, V>,
    left: Link<K, V>,
    right: Link<K, V>,
    memory: &MemoryBudget,
) -> Result<SharedNode<K, V>, MemoryError> {
    let height = 1 + height(&left).max(height(&right));
    Budgeted::new(
        Node {
            entry,
            left,
            right,
            height,
        },
        memory.empty_reservation(),
    )
    .into_shared()
}

fn balance<K, V>(
    entry: Entry<K, V>,
    left: Link<K, V>,
    right: Link<K, V>,
    memory: &MemoryBudget,
) -> Result<SharedNode<K, V>, MemoryError> {
    if height(&left) > height(&right) + 1 {
        let child = left.as_ref().expect("left-heavy tree");
        if height(&child.left) >= height(&child.right) {
            let right = make_node(entry, child.right.clone(), right, memory)?;
            return make_node(child.entry.clone(), child.left.clone(), Some(right), memory);
        }
        let pivot = child.right.as_ref().expect("left-right rotation");
        let left = make_node(
            child.entry.clone(),
            child.left.clone(),
            pivot.left.clone(),
            memory,
        )?;
        let right = make_node(entry, pivot.right.clone(), right, memory)?;
        return make_node(pivot.entry.clone(), Some(left), Some(right), memory);
    }
    if height(&right) > height(&left) + 1 {
        let child = right.as_ref().expect("right-heavy tree");
        if height(&child.right) >= height(&child.left) {
            let left = make_node(entry, left, child.left.clone(), memory)?;
            return make_node(child.entry.clone(), Some(left), child.right.clone(), memory);
        }
        let pivot = child.left.as_ref().expect("right-left rotation");
        let left = make_node(entry, left, pivot.left.clone(), memory)?;
        let right = make_node(
            child.entry.clone(),
            pivot.right.clone(),
            child.right.clone(),
            memory,
        )?;
        return make_node(pivot.entry.clone(), Some(left), Some(right), memory);
    }
    make_node(entry, left, right, memory)
}
