//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode continuations retain enough capacity to retire their native forest without allocating.

use super::{Frame, NativeNode, Node};

impl<N: NativeNode> crate::plan::retirement::RetirementFrame for Frame<N> {
    type Node = N;
    type Local = ();

    fn empty() -> N {
        N::empty()
    }
    fn is_empty(node: &N) -> bool {
        node.is_empty()
    }
    fn new(node: N) -> Self {
        Self::new(node)
    }
    fn advance_retirement(&mut self, (): &mut ()) -> bool {
        if !self.retiring {
            self.retiring = true;
            self.position = 0;
            self.count = None;
            N::retire_scalars(self);
        }
        true
    }
    fn retire_child(&mut self) -> Option<N> {
        N::retire_child(self)
    }
}

impl Frame {
    pub(super) fn retire_scalars(&mut self) {
        self.node.retire_scalars();
    }

    pub(super) fn retire_child(&mut self) -> Option<Node> {
        self.node.retire_child(&mut self.position)
    }
}
