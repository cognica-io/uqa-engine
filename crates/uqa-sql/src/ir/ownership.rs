//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar IR retires nested children without allocation or recursive stack growth.

use super::ScalarExpr;

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

pub(crate) struct Retirement {
    current: ScalarExpr,
    pending: ScalarExpr,
    ascending: bool,
    complete: bool,
}

impl Retirement {
    pub(crate) fn new(current: ScalarExpr) -> Self {
        Self {
            current,
            pending: ScalarExpr::Star,
            ascending: false,
            complete: false,
        }
    }

    // A suspended parent lends its first child slot to the preceding parent. Every other child is retired before this link is removed. The existing tree supplies all continuation storage.
    pub(crate) fn advance(&mut self) -> bool {
        if self.complete {
            return true;
        }
        if !self.ascending {
            if let Some(first) = children::first(&mut self.current) {
                std::mem::swap(first, &mut self.pending);
                std::mem::swap(&mut self.current, &mut self.pending);
            } else {
                if !metadata::advance(&mut self.current) {
                    return false;
                }
                drop(std::mem::replace(&mut self.current, ScalarExpr::Star));
                self.ascending = true;
            }
        } else if let Some(child) = remaining::take(&mut self.pending) {
            self.current = child;
            self.ascending = false;
        } else {
            if !metadata::advance(&mut self.pending) {
                return false;
            }
            if let Some(link) = children::first(&mut self.pending) {
                let previous = std::mem::replace(link, ScalarExpr::Star);
                // Only empty child slots remain in the retired parent; its ordinary field destruction cannot descend into another scalar tree.
                self.pending = previous;
            } else {
                self.complete = true;
            }
        }
        self.complete
    }
}

impl Drop for Retirement {
    fn drop(&mut self) {
        while !self.advance() {}
    }
}

#[cfg(test)]
mod tests;
