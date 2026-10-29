//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural edits move child reservations with their slots and retire discarded payloads immediately.

use super::{children, OwnedScalarExpr, Result, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

impl OwnedScalarExpr {
    /// Replace one immediate child and return the previous child with its complete independent owner. The parent retains the same box or vector slot; neither child is copied or implicitly discarded.
    pub fn replace_child(
        mut self,
        index: usize,
        replacement: Self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Self)> {
        self.assert_owner(control);
        replacement.assert_owner(control);
        control.check()?;
        let previous = self.take_child(index);
        let Self { expression, memory } = replacement;
        *children::get_mut(&mut self.expression, index).expect("scalar child index") = expression;
        if let Some(parent) = &mut self.memory {
            parent.children[index] = memory;
        }
        control.check()?;
        Ok((self, previous))
    }

    /// Exchange two immediate children without changing either container or payload ownership. Both indices use the same ordering as `map_child`.
    pub fn swap_children(
        mut self,
        left: usize,
        right: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let count = self.child_count();
        assert!(left < count && right < count, "scalar child index");
        if left != right {
            let first = std::mem::replace(
                children::get_mut(&mut self.expression, left).expect("scalar child index"),
                ScalarExpr::Literal(Value::Null),
            );
            let second = std::mem::replace(
                children::get_mut(&mut self.expression, right).expect("scalar child index"),
                first,
            );
            *children::get_mut(&mut self.expression, left).expect("scalar child index") = second;
            if let Some(memory) = &mut self.memory {
                memory.children.swap(left, right);
            }
        }
        control.check()?;
        Ok(self)
    }

    /// Retain at most `count` arguments of a function call. Ordering expressions and the filter retain their original owners; argument and metadata capacities remain with the parent.
    pub fn truncate_call_arguments(
        mut self,
        count: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func { args, .. } = &mut self.expression else {
            panic!("function argument edit requires a function call");
        };
        let removed = count.min(args.len())..args.len();
        args.truncate(count);
        self.remove_child_owners(removed);
        control.check()?;
        Ok(self)
    }

    /// Retain at most `count` CASE arms. The base and ELSE remain in their original boxes, and the original arm vector retains its capacity.
    pub fn truncate_case_arms(
        mut self,
        count: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Case { base, when, .. } = &mut self.expression else {
            panic!("CASE arm edit requires a CASE expression");
        };
        let base_count = usize::from(base.is_some());
        let removed = base_count + 2 * count.min(when.len())..base_count + 2 * when.len();
        when.truncate(count);
        self.remove_child_owners(removed);
        control.check()?;
        Ok(self)
    }

    /// Retain at most `count` AND/OR children after selection or permutation. This storage operation performs no boolean simplification.
    pub fn truncate_boolean_children(
        mut self,
        count: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = &mut self.expression else {
            panic!("boolean child edit requires AND or OR");
        };
        let removed = count.min(items.len())..items.len();
        items.truncate(count);
        self.remove_child_owners(removed);
        control.check()?;
        Ok(self)
    }

    /// Install an already owned CASE fallback. An existing ELSE box is reused; an absent box and its child-metadata slot are admitted before allocation. A failed edit destroys both private input owners.
    pub fn set_case_else(
        mut self,
        replacement: Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        replacement.assert_owner(control);
        control.check()?;
        let ScalarExpr::Case { else_branch, .. } = &mut self.expression else {
            panic!("CASE fallback edit requires a CASE expression");
        };
        if let Some(branch) = else_branch {
            let Self { expression, memory } = replacement;
            **branch = expression;
            if let Some(parent) = &mut self.memory {
                *parent.children.last_mut().expect("CASE fallback owner") = memory;
            }
        } else {
            if let Some(parent) = &mut self.memory {
                parent.children.reserve(1)?;
            }
            let boxed = control.reserve(size_of::<ScalarExpr>())?;
            let Self { expression, memory } = replacement;
            *else_branch = Some(Box::new(expression));
            if let Some(parent) = &mut self.memory {
                parent
                    .children
                    .push(memory)
                    .expect("CASE fallback slot was admitted");
                parent
                    .direct
                    .absorb(boxed.expect("controlled CASE fallback box"));
            }
        }
        control.check()?;
        Ok(self)
    }

    // The corresponding IR children have already been destroyed. Move later owners over the removed range before retiring its reservations; do not shrink the parent's still-live metadata buffer.
    pub(super) fn remove_child_owners(&mut self, removed: std::ops::Range<usize>) {
        if let Some(memory) = &mut self.memory {
            let count = removed.len();
            let retained = memory.children.len() - count;
            memory.children[removed.start..].rotate_left(count);
            memory.children.truncate(retained);
        }
    }
}
