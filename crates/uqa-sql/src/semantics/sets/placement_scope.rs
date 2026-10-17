//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whether analysis on a thread places set-returning calls again.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

thread_local! {
    static PLACING_SET_CALLS: Cell<bool> = const { Cell::new(true) };
}

/// Decides, until it drops, whether analysis on the current thread places set-returning calls, as parse analysis does for a statement or definition it analyzes for the first time, and restores the enclosing decision when it drops.
pub struct SetPlacementScope {
    previous: bool,
    thread: PhantomData<Rc<()>>,
}

impl SetPlacementScope {
    /// Analyze or run a stored definition: a view, a SQL-standard routine body or a rule, which `PostgreSQL` keeps as the tree its parse analysis produced when the definition was created and never analyzes again. Its set-returning calls are not placed again, so a definition stored by an earlier release keeps loading, and evaluating one of them where the executor cannot accept a set reports the executor's diagnostic.
    #[must_use]
    pub fn stored_definition() -> Self {
        Self::enter(false)
    }

    /// Analyze the statements a routine compiles from its source text when it runs, which `PostgreSQL` analyzes like a new statement even when a stored definition calls the routine.
    #[must_use]
    pub fn compiled_source() -> Self {
        Self::enter(true)
    }

    fn enter(placing: bool) -> Self {
        Self {
            previous: PLACING_SET_CALLS.replace(placing),
            thread: PhantomData,
        }
    }
}

impl Drop for SetPlacementScope {
    fn drop(&mut self) {
        PLACING_SET_CALLS.set(self.previous);
    }
}

/// Whether analysis on the current thread places set-returning calls, which it does outside a stored definition.
pub fn placing_set_calls() -> bool {
    PLACING_SET_CALLS.get()
}

#[cfg(test)]
mod tests {
    use super::{placing_set_calls, SetPlacementScope};

    #[test]
    fn stored_definitions_place_no_set_returning_call_until_their_scope_ends() {
        assert!(placing_set_calls());
        let stored = SetPlacementScope::stored_definition();
        assert!(!placing_set_calls());
        {
            // A routine compiled from its source inside a stored definition places its calls again.
            let _source = SetPlacementScope::compiled_source();
            assert!(placing_set_calls());
            {
                let _nested = SetPlacementScope::stored_definition();
                assert!(!placing_set_calls());
            }
            assert!(placing_set_calls());
        }
        assert!(!placing_set_calls());
        drop(stored);
        assert!(placing_set_calls());
    }
}
