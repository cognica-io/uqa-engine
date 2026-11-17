//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar IR retires nested children without allocation or recursive stack growth.

use super::ScalarExpr;
use uqa_core::memory::{RetirementNode, TreeRetirement};

mod children;
mod metadata;
mod remaining;
mod windows;

pub(crate) fn retire(value: &mut ScalarExpr) {
    if matches!(value, ScalarExpr::Star) {
        return;
    }
    drop(Retirement::new(std::mem::replace(value, ScalarExpr::Star)));
}

pub(crate) type Retirement = TreeRetirement<ScalarExpr>;

impl RetirementNode for ScalarExpr {
    type Local = metadata::Metadata;
    fn empty() -> Self {
        Self::Star
    }
    fn first_child(&mut self) -> Option<&mut Self> {
        children::first(self)
    }
    fn take_remaining_child(&mut self) -> Option<Self> {
        remaining::take(self)
    }
    fn retire_local(&mut self, local: &mut Self::Local) -> bool {
        metadata::advance(self, local)
    }
}

#[cfg(test)]
mod tests;
