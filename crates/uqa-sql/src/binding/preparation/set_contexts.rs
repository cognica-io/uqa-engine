//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning call placement in analysis order, as `PostgreSQL`'s parse analysis checks it while it transforms each expression.

use super::{Preparation, SQLError};
use crate::ast::FunctionBinding;
use crate::semantics::sets::placement::SetPlacement;

impl Preparation<'_> {
    /// Analyze `clause` as the expression kind `placement`, then restore the enclosing kind.
    pub(super) fn in_placement<T>(
        &mut self,
        placement: SetPlacement,
        clause: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let enclosing = std::mem::replace(&mut self.scope.set_placement, placement);
        let result = clause(self);
        self.scope.set_placement = enclosing;
        result
    }

    /// Analyze a nested query or data-modifying statement as a level of its own, as `parse_sub_analyze` starts a new parse state: its clauses choose their own expression kinds, and its set-returning calls belong to no expression of the enclosing level.
    pub(super) fn in_query_level<T>(
        &mut self,
        level: impl FnOnce(&mut Self) -> Result<T, SQLError>,
    ) -> Result<T, SQLError> {
        let placement = std::mem::replace(&mut self.scope.set_placement, SetPlacement::Expanding);
        let calls = self.scope.set_calls;
        let result = level(self);
        self.scope.set_placement = placement;
        self.scope.set_calls = calls;
        result
    }

    /// The set-returning calls analyzed so far at this query level.
    pub(super) fn set_calls(&self) -> usize {
        self.scope.set_calls
    }

    /// Reject a construct whose inputs, analyzed since the reading `before`, contain a set-returning call.
    pub(super) fn reject_set_inputs(
        &self,
        before: usize,
        error: impl FnOnce() -> SQLError,
    ) -> Result<(), SQLError> {
        if self.scope.set_calls == before {
            Ok(())
        } else {
            Err(error())
        }
    }

    /// Reject a set-returning call among the inputs, analyzed since the reading `before`, of a `COALESCE` or of an aggregate call, as `transformCoalesceExpr` and `check_agg_arguments` do. A set-returning call in an aggregate's FILTER was already rejected where it was analyzed.
    pub(super) fn check_set_inputs(
        &self,
        name: &str,
        selected: Option<&FunctionBinding>,
        before: usize,
    ) -> Result<(), SQLError> {
        if self.scope.set_calls == before {
            return Ok(());
        }
        if selected.is_none_or(|binding| binding.builtin)
            && crate::semantics::builtin_function_dispatch_name(name) == "coalesce"
        {
            return Err(crate::semantics::sets::placement::conditional_set_error(
                "set-returning functions are not allowed in COALESCE",
            ));
        }
        let scalar = selected
            .map(|binding| self.routines.is_scalar_function_binding(binding))
            .transpose()?
            .unwrap_or(false);
        if !scalar
            && (crate::semantics::is_builtin_aggregate_call(name, selected)
                || self.routines.has_registered_aggregate_function(name))
        {
            return Err(crate::semantics::sets::placement::conditional_set_error(
                "aggregate function calls cannot contain set-returning function calls",
            ));
        }
        Ok(())
    }

    /// Admit an analyzed call that returns a set where the current expression kind allows one, as `check_srf_call_placement` does, and record it. The reading `before` precedes the call's arguments. The routine analysis selected, or the catalog for an unselected call, classifies the call without analyzing its arguments again.
    pub(super) fn place_set_call(
        &mut self,
        name: &str,
        selected: Option<&FunctionBinding>,
        before: usize,
    ) -> Result<(), SQLError> {
        if crate::semantics::sets::static_setness::function_may_return_set_statically(
            self.routines,
            name,
            selected,
        ) {
            self.scope
                .set_placement
                .admit(self.scope.set_calls != before)?;
            self.scope.set_calls += 1;
        }
        Ok(())
    }
}
