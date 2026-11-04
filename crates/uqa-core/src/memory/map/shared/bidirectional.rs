//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Link;

/// An allocation-free ordered cursor with constant retained state. Each step seeks from the immutable root in O(log n) comparisons; mixed forward and reverse steps consume each entry at most once.
pub struct BudgetedSharedMapBidirectionalIter<'a, K, V> {
    root: &'a Link<K, V>,
    front: Option<&'a K>,
    back: Option<&'a K>,
    remaining: usize,
}

impl<'a, K: Ord, V> BudgetedSharedMapBidirectionalIter<'a, K, V> {
    pub(super) fn new(root: &'a Link<K, V>, remaining: usize) -> Self {
        Self {
            root,
            front: None,
            back: None,
            remaining,
        }
    }

    fn advance(&mut self, reverse: bool) -> Option<(&'a K, &'a V)> {
        if self.remaining == 0 {
            return None;
        }
        let previous = if reverse { self.back } else { self.front };
        let mut link = self.root;
        let mut found = None;
        while let Some(node) = link {
            let eligible = previous.is_none_or(|key| {
                if reverse {
                    node.entry.0 < *key
                } else {
                    node.entry.0 > *key
                }
            });
            if eligible {
                found = Some((&node.entry.0, &node.entry.1));
            }
            link = if eligible == reverse {
                &node.right
            } else {
                &node.left
            };
        }
        let (key, value) = found.expect("unconsumed immutable map entry");
        if reverse {
            self.back = Some(key);
        } else {
            self.front = Some(key);
        }
        self.remaining -= 1;
        Some((key, value))
    }
}

impl<'a, K: Ord, V> Iterator for BudgetedSharedMapBidirectionalIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.advance(false)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K: Ord, V> DoubleEndedIterator for BudgetedSharedMapBidirectionalIter<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.advance(true)
    }
}

impl<K: Ord, V> ExactSizeIterator for BudgetedSharedMapBidirectionalIter<'_, K, V> {}
impl<K: Ord, V> std::iter::FusedIterator for BudgetedSharedMapBidirectionalIter<'_, K, V> {}
