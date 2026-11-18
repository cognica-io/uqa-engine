//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boxed child pairs use their original optional links with the shared tree retirement machine.

use super::RetirementNode;

/// A binary topology whose child allocation contains both sibling values. Local retirement must preserve both child links and release its continuation state before returning true; the pair is destroyed after both local transitions finish.
pub trait PairedRetirementNode: Sized {
    type Local: Default;
    fn children_mut(&mut self) -> &mut Option<Box<(Self, Self)>>;
    fn retire_local(&mut self, local: &mut Self::Local) -> bool;
}

impl<N: PairedRetirementNode> RetirementNode for Option<Box<(N, N)>> {
    type Local = (bool, N::Local);

    fn empty() -> Self {
        None
    }

    fn first_child(&mut self) -> Option<&mut Self> {
        self.as_mut().map(|pair| pair.0.children_mut())
    }

    fn take_remaining_child(&mut self) -> Option<Self> {
        self.as_mut()?.1.children_mut().take().map(Some)
    }

    fn retire_local(&mut self, local: &mut Self::Local) -> bool {
        let Some(pair) = self else { return true };
        if !local.0 {
            if pair.0.retire_local(&mut local.1) {
                local.0 = true;
            }
            return false;
        }
        if pair.1.retire_local(&mut local.1) {
            local.0 = false;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
