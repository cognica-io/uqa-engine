//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar roots and their containers retain ordered progress; expression depth belongs to the existing allocation-free IR retirement state.

use super::{Retire, Retirement};

#[derive(Default)]
pub(crate) struct ScalarRetirement {
    // The deepest native field layout is MERGE clauses -> assignments -> multiple targets -> target indirection. SQL expression and query nesting do not consume these positions.
    positions: [usize; 4],
    scalar: Option<Retirement>,
}

impl std::fmt::Debug for ScalarRetirement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScalarRetirement")
            .field("positions", &self.positions)
            .field("active_scalar", &self.scalar.is_some())
            .finish()
    }
}

impl ScalarRetirement {
    pub(crate) fn advance<T: Retire>(&mut self, value: &mut T) -> bool {
        if let Some(scalar) = &mut self.scalar {
            if scalar.advance() {
                self.scalar = None;
            }
            return false;
        }
        value.retire_step(&mut self.positions, &mut self.scalar)
    }
}

#[cfg(test)]
mod tests;
