//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar syntax owns retirement independently of its parser or image producer.

use super::Expr;
use uqa_core::{
    memory::{RetirementNode, TreeRetirement},
    Value, ValueRetirement,
};

mod children;
mod remaining;
#[cfg(test)]
mod tests;
mod windows;

impl Drop for Expr {
    fn drop(&mut self) {
        let mut local = None;
        while !self.retire_local(&mut local) {}
        let mut owns_child = false;
        children::visit(self, |child| owns_child |= !matches!(child, Self::Star));
        if !owns_child {
            return;
        }

        drop(TreeRetirement::new(std::mem::replace(self, Self::Star)));
    }
}

impl RetirementNode for Expr {
    type Local = Option<ValueRetirement>;
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
        if let Some(retirement) = local {
            if retirement.advance() {
                *local = None;
            }
            return false;
        }
        let value = match self {
            Self::Literal(value) => Some(value),
            Self::TypedLiteral {
                value,
                composite_source,
                ..
            } => {
                if matches!(value, Value::Null) {
                    composite_source
                        .as_deref_mut()
                        .map(|source| &mut source.value)
                } else {
                    Some(value)
                }
            }
            _ => None,
        };
        if let Some(value) = value.filter(|value| !matches!(value, Value::Null)) {
            *local = Some(ValueRetirement::take(value));
            false
        } else {
            true
        }
    }
}
