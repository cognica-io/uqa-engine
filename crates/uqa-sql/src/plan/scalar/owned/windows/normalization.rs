//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window calls share one pre-order traversal across inline and suspended execution.

use super::{
    CatalogRetentionError, OwnedScalarExpr, ProductionControl, ScalarExpr, ScalarWindowSpec, Work,
};
use crate::{construction::Stack, plan::scalar::owned::replacement::visit_index, SQLError};

struct Parent {
    expression: OwnedScalarExpr,
    child: usize,
    next: usize,
}

impl OwnedScalarExpr {
    pub(in crate::plan) async fn normalize_window_definitions_from<W: Work>(
        mut self,
        resolved: &[ScalarWindowSpec],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, SQLError>
    where
        W::Error: From<CatalogRetentionError> + Into<SQLError>,
    {
        self.assert_owner(control);
        let mut parents = Stack::<_, 4>::new(*control);
        let mut entering = true;
        let mut ordinal = 0;
        loop {
            super::replacement::checkpoint(control, work)
                .await
                .map_err(Into::into)?;
            if entering {
                if let ScalarExpr::WindowCall { spec, .. } = &*self {
                    if let Some(slot) = spec.definition {
                        let canonical = resolved.get(slot).ok_or_else(|| {
                            crate::plan::windows::invalid(
                                "a call refers to a missing window definition",
                            )
                        })?;
                        self = self
                            .replace_window_spec_from(canonical, control, work)
                            .await
                            .map_err(Into::into)?;
                    }
                }
                entering = false;
            }
            let count = self.child_count();
            if ordinal < count {
                let child = visit_index(&self, ordinal, count);
                let value = self.take_child(child);
                parents.push(Parent {
                    expression: self,
                    child,
                    next: ordinal + 1,
                })?;
                self = value;
                ordinal = 0;
                entering = true;
            } else if let Some(mut parent) = parents.pop() {
                parent.expression.restore_child(parent.child, self);
                self = parent.expression;
                ordinal = parent.next;
            } else {
                control.check()?;
                return Ok(self);
            }
        }
    }
}
