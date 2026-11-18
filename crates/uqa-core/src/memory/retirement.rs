//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free owned tree retirement uses the original child slots for continuation storage.

mod paired;
pub use paired::PairedRetirementNode;

/// Native ownership adapters select children without allocating or visiting descendants. The first child slot remains available until all other children have been detached; local retirement must leave that slot and its ancestor link intact. A completed local transition releases its continuation state before another node uses it.
pub trait RetirementNode: Sized {
    type Local: Default;
    fn empty() -> Self;
    fn first_child(&mut self) -> Option<&mut Self>;
    fn take_remaining_child(&mut self) -> Option<Self>;
    fn retire_local(&mut self, local: &mut Self::Local) -> bool;
}

/// A private tree keeps its own continuation links until all payloads have retired. Productive steps and terminal destruction drive the same transitions; no separate traversal stack is allocated.
pub struct TreeRetirement<N: RetirementNode> {
    current: N,
    pending: N,
    local: N::Local,
    ascending: bool,
    complete: bool,
}

impl<N: RetirementNode> TreeRetirement<N> {
    /// Move a private payload into retirement while leaving its original slot empty.
    pub fn take(value: &mut N) -> Self {
        Self::new(std::mem::replace(value, N::empty()))
    }

    pub fn new(current: N) -> Self {
        Self {
            current,
            pending: N::empty(),
            local: N::Local::default(),
            ascending: false,
            complete: false,
        }
    }

    /// Detach one tree edge or advance the current node's local retirement. The owning adapter determines the work of a local transition; completion remains terminal on subsequent calls.
    pub fn advance(&mut self) -> bool {
        if self.complete {
            return true;
        }
        if !self.ascending {
            if let Some(first) = self.current.first_child() {
                std::mem::swap(first, &mut self.pending);
                std::mem::swap(&mut self.current, &mut self.pending);
            } else {
                if !self.current.retire_local(&mut self.local) {
                    return false;
                }
                drop(std::mem::replace(&mut self.current, N::empty()));
                self.ascending = true;
            }
        } else if let Some(child) = self.pending.take_remaining_child() {
            self.current = child;
            self.ascending = false;
        } else {
            if !self.pending.retire_local(&mut self.local) {
                return false;
            }
            if let Some(link) = self.pending.first_child() {
                let previous = std::mem::replace(link, N::empty());
                // Only empty child slots remain; ordinary field destruction cannot descend into another owned tree.
                self.pending = previous;
            } else {
                self.complete = true;
            }
        }
        self.complete
    }
}

impl<N: RetirementNode> Drop for TreeRetirement<N> {
    fn drop(&mut self) {
        while !self.advance() {}
    }
}
