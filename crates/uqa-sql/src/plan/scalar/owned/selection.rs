//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected child rewrites retain parent metadata and transfer only constructor-owned buffers.

use super::{children, OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::schema::retention::CatalogRetentionError;
use uqa_core::memory::{BudgetedVec, ProductionControl};

impl OwnedScalarExpr {
    /// Remove validated call-argument markers by transferring their value child and immediately retiring marker metadata and containers.
    pub fn into_call_argument(
        mut self,
        control: &ProductionControl<'_>,
    ) -> std::result::Result<Self, crate::SQLError> {
        self.assert_owner(control);
        control.check()?;
        crate::scalar_call_argument(&self)?;
        while let ScalarExpr::Func { binding, args, .. } = &self.expression {
            if !binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .is_some_and(crate::ast::FunctionDispatch::is_call_argument_marker)
            {
                break;
            }
            let index = args.len() - 1;
            self = self.select_child(index);
        }
        control.check()?;
        Ok(self)
    }

    /// Rewrite one child while borrowing its parent's metadata. The selected child slot contains a NULL placeholder during the callback; all other parent fields remain unchanged and read-only.
    pub fn map_child_with_parent<E>(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&ScalarExpr, Self) -> std::result::Result<Self, E>,
    ) -> std::result::Result<Self, E>
    where
        E: From<CatalogRetentionError>,
    {
        self.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        let child = self.take_child(index);
        let replacement = map(&self.expression, child)?;
        replacement.assert_owner(control);
        control.check().map_err(CatalogRetentionError::from)?;
        let Self { expression, memory } = replacement;
        *children::get_mut(&mut self.expression, index).expect("scalar child index") = expression;
        if let Some(parent) = &mut self.memory {
            parent.children[index] = memory;
        }
        Ok(self)
    }

    /// Move a call's argument vector into an OR container while retaining its call metadata, ordering expressions and filter in the returned parent. The vector's exact constructor capacity transfers with it; no argument payload is copied.
    pub fn take_call_arguments(mut self, control: &ProductionControl<'_>) -> Result<(Self, Self)> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func { args, .. } = &mut self.expression else {
            panic!("argument selection requires a function call");
        };
        let count = args.len();
        let mut selected_memory = self.memory.as_ref().map(|parent| ScalarMemory {
            direct: parent.direct.budget().empty_reservation(),
            children: BudgetedVec::new(parent.direct.budget()),
        });
        if let Some(selected) = &mut selected_memory {
            selected.children.reserve(count)?;
        }
        let arguments = std::mem::take(args);
        let selected = Self {
            expression: ScalarExpr::Or(arguments),
            memory: selected_memory,
        };
        let mut selected = selected;
        if let (Some(parent), Some(output)) = (&mut self.memory, &mut selected.memory) {
            let ScalarExpr::Or(arguments) = &selected.expression else {
                unreachable!();
            };
            output.direct = parent
                .direct
                .split(arguments.capacity() * size_of::<ScalarExpr>());
            for child in &mut parent.children[..count] {
                output
                    .children
                    .push(child.take())
                    .expect("admitted selected argument slot");
            }
            parent.children.rotate_left(count);
            let remaining = parent.children.len() - count;
            parent.children.truncate(remaining);
        }
        control.check()?;
        Ok((self, selected))
    }

    /// Replace a call's argument vector with an independently owned AND/OR vector. The boolean container's children become call arguments without evaluation; the old argument storage is destroyed before its exact lease is released.
    pub fn replace_call_arguments(
        mut self,
        replacement: Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        replacement.assert_owner(control);
        control.check()?;
        let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = &replacement.expression else {
            panic!("argument replacement requires a boolean container");
        };
        let count = items.len();
        let ScalarExpr::Func { args, .. } = &mut self.expression else {
            panic!("argument replacement requires a function call");
        };
        let previous_count = args.len();
        if let Some(parent) = &mut self.memory {
            parent
                .children
                .reserve(count.saturating_sub(previous_count))?;
        }
        let previous_bytes = args.capacity() * size_of::<ScalarExpr>();
        let Self { expression, memory } = replacement;
        let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = expression else {
            unreachable!();
        };
        *args = items;
        if let (Some(parent), Some(mut replacement)) = (&mut self.memory, memory) {
            drop(parent.direct.split(previous_bytes));
            for child in &mut parent.children[..previous_count] {
                *child = None;
            }
            parent.children.rotate_left(previous_count);
            let retained = parent.children.len() - previous_count;
            parent.children.truncate(retained);
            for child in &mut *replacement.children {
                parent
                    .children
                    .push(child.take())
                    .expect("admitted replacement argument slot");
            }
            parent.children.rotate_right(count);
            parent.direct.absorb(replacement.direct);
        }
        control.check()?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
