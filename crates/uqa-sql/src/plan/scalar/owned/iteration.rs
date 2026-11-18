//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::OwnedScalarExpr;
use crate::plan::{retirement::Pending, scalar::ScalarInputWork as Work};
use uqa_core::memory::ProductionControl;

/// Moves immediate scalar children in copy order without copying their payloads or allocating a work queue. Unvisited children and the original parent containers remain owned until exhaustion or iterator destruction.
pub struct OwnedScalarChildren {
    parent: Option<OwnedScalarExpr>,
    next: usize,
    end: usize,
}

impl OwnedScalarExpr {
    pub(crate) async fn retire_from<W: Work>(
        self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), W::Error> {
        self.assert_owner(control);
        let (expression, memory) = self.into_rewrite_parts();
        super::publication::Publication::from_parts(Pending::new(expression), memory)
            .retire_from(control, work)
            .await
    }

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

impl OwnedScalarChildren {
    fn take_next(&mut self) -> Option<OwnedScalarExpr> {
        if self.next == self.end {
            return None;
        }
        let parent = self.parent.as_mut()?;
        let child = parent.take_child(self.next);
        self.next += 1;
        Some(child)
    }

    pub(crate) async fn next_from<W: Work>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<OwnedScalarExpr>, W::Error> {
        crate::plan::retirement::windows::checkpoint(control, work).await?;
        let child = self.take_next();
        if self.next == self.end {
            if let Some(parent) = self.parent.take() {
                parent.retire_from(control, work).await?;
            }
        }
        Ok(child)
    }
}

impl Iterator for OwnedScalarChildren {
    type Item = OwnedScalarExpr;

    fn next(&mut self) -> Option<Self::Item> {
        let child = self.take_next();
        if self.next == self.end {
            self.parent = None;
        }
        child
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for OwnedScalarChildren {}
impl std::iter::FusedIterator for OwnedScalarChildren {}
