//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::OwnedScalarExpr;

/// Moves immediate scalar children in copy order without copying their payloads or allocating a work queue. Unvisited children and the original parent containers remain owned until exhaustion or iterator destruction.
pub struct OwnedScalarChildren {
    parent: Option<OwnedScalarExpr>,
    next: usize,
    end: usize,
}

impl OwnedScalarExpr {
    /// Consume the parent while moving its children. Selecting the final child immediately destroys the now-empty parent; abandoning the iterator destroys the unvisited children as well.
    pub fn into_children(self) -> OwnedScalarChildren {
        let end = self.child_count();
        OwnedScalarChildren {
            parent: (end != 0).then_some(self),
            next: 0,
            end,
        }
    }
}

impl Iterator for OwnedScalarChildren {
    type Item = OwnedScalarExpr;

    fn next(&mut self) -> Option<Self::Item> {
        let parent = self.parent.as_mut()?;
        let child = parent.take_child(self.next);
        self.next += 1;
        if self.next == self.end {
            self.parent = None;
        }
        Some(child)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for OwnedScalarChildren {}
impl std::iter::FusedIterator for OwnedScalarChildren {}
