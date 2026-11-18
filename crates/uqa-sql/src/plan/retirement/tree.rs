//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A native tree retains its admitted traversal capacity through complete and partial retirement.

use crate::construction::Stack;
use uqa_core::memory::MemoryBudget;

pub(crate) trait RetirementFrame: Sized {
    type Node;
    type Local: Default;
    fn empty() -> Self::Node;
    fn is_empty(node: &Self::Node) -> bool;
    fn new(node: Self::Node) -> Self;
    fn advance_retirement(&mut self, local: &mut Self::Local) -> bool;
    fn retire_child(&mut self) -> Option<Self::Node>;
}

#[derive(Debug)]
pub(crate) struct RetainedTree<F: RetirementFrame> {
    pub(crate) root: F::Node,
    pub(crate) frames: Stack<F>,
    local: F::Local,
}

impl<F: RetirementFrame> RetainedTree<F> {
    pub(crate) fn new(memory: &MemoryBudget) -> Self {
        Self::with_budget(Some(memory))
    }

    pub(crate) fn with_budget(memory: Option<&MemoryBudget>) -> Self {
        Self {
            root: F::empty(),
            frames: Stack::with_budget(memory),
            local: F::Local::default(),
        }
    }
}

impl<F: RetirementFrame> Drop for RetainedTree<F> {
    fn drop(&mut self) {
        self.retire();
    }
}

impl<F: RetirementFrame> RetainedTree<F> {
    pub(crate) fn retire(&mut self) {
        while !self.advance() {}
    }

    /// Retire one structural frame using the same admitted capacity as terminal cleanup.
    pub(crate) fn advance(&mut self) -> bool {
        if !F::is_empty(&self.root) {
            debug_assert!(self.frames.is_empty());
            let root = std::mem::replace(&mut self.root, F::empty());
            self.frames
                .push(F::new(root))
                .expect("admitted root retirement slot");
            return false;
        }
        if let Some(mut frame) = self.frames.pop() {
            if !frame.advance_retirement(&mut self.local) {
                self.frames
                    .push(frame)
                    .expect("admitted local retirement slot");
                return false;
            }
            if let Some(child) = frame.retire_child() {
                debug_assert!(self.frames.capacity() >= self.frames.len() + 2);
                self.frames
                    .push(frame)
                    .expect("admitted parent retirement slot");
                self.frames
                    .push(F::new(child))
                    .expect("admitted child retirement slot");
            }
        }
        self.frames.is_empty()
    }
}
