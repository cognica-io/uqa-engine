//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable deletion copies only the search/successor paths and rotation pivots.

use std::borrow::Borrow;

use super::{
    balance, height, make_node, private, unique, Budgeted, Entry, Link, MemoryBudget, MemoryError,
    MemoryReservation, Node, Ordering, SharedNode, MAX_HEIGHT,
};

pub(crate) fn remove<K: Borrow<Q>, V, Q: Ord + ?Sized>(
    root: &Link<K, V>,
    key: &Q,
    memory: &MemoryBudget,
) -> Result<(Link<K, V>, bool), MemoryError> {
    let mut path = [Ordering::Equal; MAX_HEIGHT];
    let mut depth = 0;
    let mut link = root;
    let selected = loop {
        let Some(node) = link else {
            return Ok((root.clone(), false));
        };
        let direction = key.cmp(node.entry.0.borrow());
        path[depth] = direction;
        depth += 1;
        link = match direction {
            Ordering::Less => &node.left,
            Ordering::Greater => &node.right,
            Ordering::Equal => break node,
        };
    };
    // Each reconstructed level needs its own node and at most two off-path rotation pivots. The removed node itself needs no copy when a sole child can be lifted directly.
    let mut levels = depth - 1;
    if selected.left.is_some() {
        if let Some(mut successor) = selected.right.as_ref() {
            levels += 1;
            while let Some(left) = successor.left.as_ref() {
                levels += 1;
                successor = left;
            }
        }
    }
    let bytes = levels
        .checked_mul(3 * size_of::<Budgeted<Node<K, V>>>())
        .ok_or(MemoryError::SizeOverflow)?;
    let mut allowance = memory.reserve(bytes)?;
    // Comparisons and admission precede construction; no user code runs while building the successor root.
    let root = remove_prepared(
        root.as_ref().expect("selected source root"),
        &path[..depth],
        &mut allowance,
    );
    Ok((root, true))
}

fn remove_prepared<K, V>(
    node: &SharedNode<K, V>,
    path: &[Ordering],
    allowance: &mut MemoryReservation,
) -> Link<K, V> {
    let (direction, remaining) = path.split_first().expect("selected removal path");
    let (entry, left, right) = match direction {
        Ordering::Less => (
            node.entry.clone(),
            remove_prepared(
                node.left.as_ref().expect("left removal"),
                remaining,
                allowance,
            ),
            node.right.clone(),
        ),
        Ordering::Greater => (
            node.entry.clone(),
            node.left.clone(),
            remove_prepared(
                node.right.as_ref().expect("right removal"),
                remaining,
                allowance,
            ),
        ),
        Ordering::Equal => match (&node.left, &node.right) {
            (None, right) => return right.clone(),
            (left, None) => return left.clone(),
            (left, Some(right)) => {
                let (right, successor) = remove_first(right, allowance);
                (successor, left.clone(), right)
            }
        },
    };
    Some(balance_removed(
        make_node(entry, left, right, allowance),
        allowance,
    ))
}

fn remove_first<K, V>(
    node: &SharedNode<K, V>,
    allowance: &mut MemoryReservation,
) -> (Link<K, V>, Entry<K, V>) {
    let Some(left) = &node.left else {
        return (node.right.clone(), node.entry.clone());
    };
    let (left, first) = remove_first(left, allowance);
    let node = make_node(node.entry.clone(), left, node.right.clone(), allowance);
    (Some(balance_removed(node, allowance)), first)
}

fn balance_removed<K, V>(
    mut node: SharedNode<K, V>,
    allowance: &mut MemoryReservation,
) -> SharedNode<K, V> {
    // Unlike insertion, deletion can rotate a sibling that is outside the copied search path. Admit and detach those pivots before the existing private rotations.
    if height(&node.left) > height(&node.right) + 1 {
        let child = private(&mut node).left.take().expect("left-heavy removal");
        let mut child = unique(child, allowance);
        if height(&child.left) < height(&child.right) {
            let pivot = private(&mut child)
                .right
                .take()
                .expect("double removal rotation");
            private(&mut child).right = Some(unique(pivot, allowance));
        }
        private(&mut node).left = Some(child);
    } else if height(&node.right) > height(&node.left) + 1 {
        let child = private(&mut node)
            .right
            .take()
            .expect("right-heavy removal");
        let mut child = unique(child, allowance);
        if height(&child.right) < height(&child.left) {
            let pivot = private(&mut child)
                .left
                .take()
                .expect("double removal rotation");
            private(&mut child).left = Some(unique(pivot, allowance));
        }
        private(&mut node).right = Some(child);
    }
    balance(node)
}
