//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable value traversal borrows disjoint node fields and retains the AVL path on the stack.

use super::{Link, Node, MAX_HEIGHT};

struct Visit<'a, K, V> {
    key: &'a K,
    value: &'a mut V,
    right: &'a mut Link<K, V>,
}

pub(super) struct IterMut<'a, K, V> {
    stack: [Option<Visit<'a, K, V>>; MAX_HEIGHT],
    depth: usize,
    remaining: usize,
}

impl<'a, K, V> IterMut<'a, K, V> {
    pub(super) fn new(root: &'a mut Link<K, V>, len: usize) -> Self {
        let mut output = Self {
            stack: std::array::from_fn(|_| None),
            depth: 0,
            remaining: len,
        };
        output.push_left(root);
        output
    }

    fn push_left(&mut self, mut root: &'a mut Link<K, V>) {
        while let Some(node) = root {
            let Node {
                key,
                value,
                left,
                right,
                ..
            } = &mut *node.value;
            self.stack[self.depth] = Some(Visit { key, value, right });
            self.depth += 1;
            root = left;
        }
    }
}

impl<'a, K, V> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);
    fn next(&mut self) -> Option<Self::Item> {
        self.depth = self.depth.checked_sub(1)?;
        let Visit { key, value, right } = self.stack[self.depth].take().expect("pending map node");
        self.push_left(right);
        self.remaining -= 1;
        Some((key, value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<K, V> ExactSizeIterator for IterMut<'_, K, V> {}
impl<K, V> std::iter::FusedIterator for IterMut<'_, K, V> {}
