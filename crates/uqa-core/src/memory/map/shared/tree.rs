//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit the complete AVL insertion before mutation; reuse private nodes and rotations.

use std::{cmp::Ordering, sync::Arc};

use super::{Budgeted, Entry, Link, MemoryBudget, MemoryError, Node, SharedNode, MAX_HEIGHT};
use crate::memory::MemoryReservation;

pub(super) fn insert<K: Ord, V>(
    root: &mut Link<K, V>,
    key: K,
    value: V,
    memory: &MemoryBudget,
) -> Result<bool, MemoryError> {
    let mut path = [Ordering::Equal; MAX_HEIGHT];
    let mut depth = 0;
    let mut required = size_of::<Budgeted<(K, V)>>();
    let mut shared = false;
    let mut link = &*root;
    while let Some(node) = link {
        // Copying a shared ancestor also shares each remaining node on this path.
        shared |= Arc::strong_count(node) != 1 || Arc::weak_count(node) != 0;
        if shared {
            required = required
                .checked_add(size_of::<Budgeted<Node<K, V>>>())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let direction = key.cmp(&node.entry.0);
        path[depth] = direction;
        depth += 1;
        link = match direction {
            Ordering::Less => &node.left,
            Ordering::Greater => &node.right,
            Ordering::Equal => break,
        };
    }
    if link.is_none() {
        required = required
            .checked_add(size_of::<Budgeted<Node<K, V>>>())
            .ok_or(MemoryError::SizeOverflow)?;
    } else if !shared
        && link.as_ref().is_some_and(|node| {
            Arc::strong_count(&node.entry) == 1 && Arc::weak_count(&node.entry) == 0
        })
    {
        let mut link = &mut *root;
        for direction in &path[..depth] {
            let node = private(link.as_mut().expect("admitted replacement path"));
            match direction {
                Ordering::Less => link = &mut node.left,
                Ordering::Greater => link = &mut node.right,
                Ordering::Equal => {
                    let entry = Arc::get_mut(&mut node.entry).expect("private replacement entry");
                    let previous = std::mem::replace(&mut entry.value, (key, value));
                    drop(previous);
                    return Ok(false);
                }
            }
        }
        unreachable!("replacement path ends at an equal key");
    }
    let mut allowance = memory.reserve(required)?;
    let entry = Arc::new(Budgeted::new(
        (key, value),
        allowance.split(size_of::<Budgeted<(K, V)>>()),
    ));
    // Comparisons and admission are complete; publication cannot fail or call Ord again.
    let (inserted, replaced) = insert_prepared(root.take(), entry, &path[..depth], &mut allowance);
    let added = replaced.is_none();
    *root = Some(inserted);
    drop(replaced);
    Ok(added)
}

fn insert_prepared<K, V>(
    link: Link<K, V>,
    entry: Entry<K, V>,
    path: &[Ordering],
    allowance: &mut MemoryReservation,
) -> (SharedNode<K, V>, Option<Entry<K, V>>) {
    let Some(node) = link else {
        return (make_node(entry, None, None, allowance), None);
    };
    let mut node = unique(node, allowance);
    let (direction, remaining) = path.split_first().expect("admitted insertion path");
    let replaced = match direction {
        Ordering::Less => {
            let (left, replaced) =
                insert_prepared(private(&mut node).left.take(), entry, remaining, allowance);
            private(&mut node).left = Some(left);
            replaced
        }
        Ordering::Greater => {
            let (right, replaced) =
                insert_prepared(private(&mut node).right.take(), entry, remaining, allowance);
            private(&mut node).right = Some(right);
            replaced
        }
        Ordering::Equal => {
            let replaced = std::mem::replace(&mut private(&mut node).entry, entry);
            return (node, Some(replaced));
        }
    };
    (balance(node), replaced)
}

fn height<K, V>(link: &Link<K, V>) -> u8 {
    link.as_ref().map_or(0, |node| node.height)
}

fn make_node<K, V>(
    entry: Entry<K, V>,
    left: Link<K, V>,
    right: Link<K, V>,
    allowance: &mut MemoryReservation,
) -> SharedNode<K, V> {
    let height = 1 + height(&left).max(height(&right));
    Arc::new(Budgeted::new(
        Node {
            entry,
            left,
            right,
            height,
        },
        allowance.split(size_of::<Budgeted<Node<K, V>>>()),
    ))
}

fn unique<K, V>(mut node: SharedNode<K, V>, allowance: &mut MemoryReservation) -> SharedNode<K, V> {
    if Arc::get_mut(&mut node).is_some() {
        node
    } else {
        make_node(
            node.entry.clone(),
            node.left.clone(),
            node.right.clone(),
            allowance,
        )
    }
}

fn private<K, V>(node: &mut SharedNode<K, V>) -> &mut Node<K, V> {
    &mut Arc::get_mut(node).expect("private insertion path").value
}

fn update_height<K, V>(node: &mut SharedNode<K, V>) {
    let node = private(node);
    node.height = 1 + height(&node.left).max(height(&node.right));
}

fn balance<K, V>(mut node: SharedNode<K, V>) -> SharedNode<K, V> {
    update_height(&mut node);
    // Insertion rotations use only pivots on the already private search path.
    if height(&node.left) > height(&node.right) + 1 {
        let child = node.left.as_ref().expect("left-heavy tree");
        if height(&child.left) < height(&child.right) {
            let child = private(&mut node).left.take().expect("left-heavy tree");
            private(&mut node).left = Some(rotate_left(child));
        }
        return rotate_right(node);
    }
    if height(&node.right) > height(&node.left) + 1 {
        let child = node.right.as_ref().expect("right-heavy tree");
        if height(&child.right) < height(&child.left) {
            let child = private(&mut node).right.take().expect("right-heavy tree");
            private(&mut node).right = Some(rotate_right(child));
        }
        return rotate_left(node);
    }
    node
}

fn rotate_left<K, V>(mut node: SharedNode<K, V>) -> SharedNode<K, V> {
    let mut pivot = private(&mut node).right.take().expect("left rotation");
    private(&mut node).right = private(&mut pivot).left.take();
    update_height(&mut node);
    private(&mut pivot).left = Some(node);
    update_height(&mut pivot);
    pivot
}

fn rotate_right<K, V>(mut node: SharedNode<K, V>) -> SharedNode<K, V> {
    let mut pivot = private(&mut node).left.take().expect("right rotation");
    private(&mut node).left = private(&mut pivot).right.take();
    update_height(&mut node);
    private(&mut pivot).right = Some(node);
    update_height(&mut pivot);
    pivot
}
