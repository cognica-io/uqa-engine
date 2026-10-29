//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-index changes are allocation-free and retain every scalar constructor.

use super::{OwnedScalarExpr, ScalarExpr};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl OwnedScalarExpr {
    pub(crate) fn remap_query_occurrences(
        mut self,
        control: &ProductionControl<'_>,
        map: &mut dyn FnMut(usize) -> Result<usize, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        let mut result: Result<(), SQLError> = Ok(());
        crate::plan::rewrite_scalar_expression(&mut self.expression, &mut |node| {
            if result.is_err() {
                return;
            }
            result = (|| {
                control.check()?;
                match node {
                    ScalarExpr::ScalarSubquery(id)
                    | ScalarExpr::Exists { subquery: id, .. }
                    | ScalarExpr::InSubquery { subquery: id, .. } => *id = map(*id)?,
                    _ => {}
                }
                Ok(())
            })();
        });
        result?;
        Ok(self)
    }

    /// Remap only this expression's local scalar-query references after their query owners have moved together. Nested query arenas are not part of the scalar tree.
    pub fn remap_subquery_ids(
        mut self,
        control: &ProductionControl<'_>,
        map: &mut dyn FnMut(usize) -> usize,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        let mut checked = Ok(());
        self.expression.visit_mut(&mut |node| {
            if checked.is_ok() {
                checked = control.check();
                match node {
                    ScalarExpr::ScalarSubquery(id)
                    | ScalarExpr::Exists { subquery: id, .. }
                    | ScalarExpr::InSubquery { subquery: id, .. } => *id = map(*id),
                    _ => {}
                }
            }
        });
        checked?;
        Ok(self)
    }
}
