//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar IR retires nested children without allocation or recursive stack growth.

use super::ScalarExpr;

mod children;
mod remaining;
mod windows;

pub(crate) fn retire(value: &mut ScalarExpr) {
    let mut owns_child = false;
    children::visit(value, |child| {
        owns_child |= !matches!(child, ScalarExpr::Star);
    });
    if !owns_child {
        return;
    }

    // A suspended parent lends its first child slot to the preceding parent. Every other child is retired before this link is removed. The existing tree supplies all continuation storage.
    let mut pending = ScalarExpr::Star;
    let mut current = std::mem::replace(value, ScalarExpr::Star);
    loop {
        if let Some(first) = children::first(&mut current) {
            std::mem::swap(first, &mut pending);
            std::mem::swap(&mut current, &mut pending);
            continue;
        }
        drop(current);
        loop {
            if let Some(child) = remaining::take(&mut pending) {
                current = child;
                break;
            }
            let Some(link) = children::first(&mut pending) else {
                return;
            };
            let previous = std::mem::replace(link, ScalarExpr::Star);
            // Only empty child slots remain in the retired parent; its ordinary field destruction cannot descend into another scalar tree.
            pending = previous;
        }
    }
}
