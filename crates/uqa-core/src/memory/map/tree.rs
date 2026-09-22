//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rotations and removals move owned nodes without allocating or cloning keys and values.

use std::{borrow::Borrow, cmp::Ordering};

use super::{Link, Node, OwnedNode};

pub(super) fn get<'a, K: Borrow<Q>, V, Q: Ord + ?Sized>(
    mut link: &'a Link<K, V>,
    key: &Q,
) -> Option<&'a Node<K, V>> {
    while let Some(node) = link {
        match key.cmp(node.key.borrow()) {
            Ordering::Less => link = &node.left,
            Ordering::Greater => link = &node.right,
            Ordering::Equal => return Some(node),
        }
    }
    None
}

pub(super) fn get_mut<'a, K: Borrow<Q>, V, Q: Ord + ?Sized>(
    mut link: &'a mut Link<K, V>,
    key: &Q,
) -> Option<&'a mut Node<K, V>> {
    while let Some(node) = link {
        let node = &mut *node.value;
        match key.cmp(node.key.borrow()) {
            Ordering::Less => link = &mut node.left,
            Ordering::Greater => link = &mut node.right,
            Ordering::Equal => return Some(node),
        }
    }
    None
}

pub(super) fn insert<K: Ord, V>(link: &mut Link<K, V>, entry: OwnedNode<K, V>) -> Option<V> {
    let Some(node) = link else {
        *link = Some(entry);
        return None;
    };
    let node = &mut *node.value;
    let previous = match entry.key.cmp(&node.key) {
        Ordering::Less => insert(&mut node.left, entry),
        Ordering::Greater => insert(&mut node.right, entry),
        Ordering::Equal => {
            let (_, value) = into_entry(entry);
            return Some(std::mem::replace(&mut node.value, value));
        }
    };
    balance(link);
    previous
}

pub(super) fn remove<K: Borrow<Q>, V, Q: Ord + ?Sized>(
    link: &mut Link<K, V>,
    key: &Q,
) -> Option<(K, V)> {
    let node = link.as_mut()?;
    let removed = match key.cmp(node.key.borrow()) {
        Ordering::Less => remove(&mut node.value.left, key),
        Ordering::Greater => remove(&mut node.value.right, key),
        Ordering::Equal => {
            let mut node = link.take().expect("selected removal node");
            *link = if node.right.is_none() {
                node.value.left.take()
            } else {
                let mut successor = take_first(&mut node.value.right);
                successor.value.left = node.value.left.take();
                successor.value.right = node.value.right.take();
                Some(successor)
            };
            Some(into_entry(node))
        }
    };
    if removed.is_some() {
        balance(link);
    }
    removed
}

fn take_first<K, V>(link: &mut Link<K, V>) -> OwnedNode<K, V> {
    let node = link.as_mut().expect("nonempty successor tree");
    if node.left.is_none() {
        let mut node = link.take().expect("first successor node");
        *link = node.value.right.take();
        return node;
    }
    let first = take_first(&mut node.value.left);
    balance(link);
    first
}

fn into_entry<K, V>(node: OwnedNode<K, V>) -> (K, V) {
    let (node, memory) = node.into_parts();
    let entry = unbox_entry(node);
    // The callee has freed the node allocation before its reservation is released.
    drop(memory);
    entry
}

#[expect(
    clippy::boxed_local,
    reason = "Consume and free the node allocation before the caller releases its reservation."
)]
fn unbox_entry<K, V>(node: Box<Node<K, V>>) -> (K, V) {
    let Node { key, value, .. } = *node;
    (key, value)
}

fn height<K, V>(link: &Link<K, V>) -> u8 {
    link.as_ref().map_or(0, |node| node.height)
}

fn update_height<K, V>(node: &mut Node<K, V>) {
    node.height = 1 + height(&node.left).max(height(&node.right));
}

fn balance<K, V>(link: &mut Link<K, V>) {
    let Some(node) = link else { return };
    update_height(&mut node.value);
    if height(&node.left) > height(&node.right) + 1 {
        let child = node.left.as_ref().expect("left-heavy tree");
        if height(&child.right) > height(&child.left) {
            rotate_left(&mut node.value.left);
        }
        rotate_right(link);
    } else if height(&node.right) > height(&node.left) + 1 {
        let child = node.right.as_ref().expect("right-heavy tree");
        if height(&child.left) > height(&child.right) {
            rotate_right(&mut node.value.right);
        }
        rotate_left(link);
    }
}

fn rotate_left<K, V>(link: &mut Link<K, V>) {
    let mut root = link.take().expect("rotation root");
    let mut next = root.value.right.take().expect("rotation right child");
    root.value.right = next.value.left.take();
    update_height(&mut root.value);
    next.value.left = Some(root);
    update_height(&mut next.value);
    *link = Some(next);
}

fn rotate_right<K, V>(link: &mut Link<K, V>) {
    let mut root = link.take().expect("rotation root");
    let mut next = root.value.left.take().expect("rotation left child");
    root.value.left = next.value.right.take();
    update_height(&mut root.value);
    next.value.right = Some(root);
    update_height(&mut next.value);
    *link = Some(next);
}

pub(super) fn for_each_mut<K, V>(link: &mut Link<K, V>, visit: &mut impl FnMut(&K, &mut V)) {
    if let Some(node) = link {
        let node = &mut *node.value;
        for_each_mut(&mut node.left, visit);
        visit(&node.key, &mut node.value);
        for_each_mut(&mut node.right, visit);
    }
}
