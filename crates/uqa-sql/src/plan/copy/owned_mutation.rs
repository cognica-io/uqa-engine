//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View mutation metadata adopts independently produced names and scalar fields.

use super::{Copying, Result};
use crate::plan::{OwnedScalarExpr, ViewCheckPlan, ViewRuleInsertPlan, ViewRuleUpdatePlan};
use uqa_core::memory::Produced;

impl Copying<'_> {
    pub(in crate::plan) fn owned_view_check(
        &mut self,
        view: Produced<String>,
        predicate: OwnedScalarExpr,
    ) -> Result<ViewCheckPlan> {
        Ok(ViewCheckPlan {
            view: self.take(view),
            predicate: self.field("predicate", |copying| copying.take_scalar(predicate))?,
        })
    }

    pub(in crate::plan) fn owned_view_update_inputs(
        &mut self,
        relation: Produced<String>,
        assigned: Produced<Vec<String>>,
    ) -> Result<ViewRuleUpdatePlan> {
        Ok(ViewRuleUpdatePlan {
            relation: self.take(relation),
            assigned_columns: self
                .field("assigned_columns", |copying| Ok(copying.take(assigned)))?,
            input_columns: self.field("input_columns", |_| Ok(Vec::new()))?,
        })
    }
    pub(in crate::plan) fn owned_view_insert_inputs(
        &mut self,
        relation: Produced<String>,
        assigned: Produced<Vec<String>>,
    ) -> Result<ViewRuleInsertPlan> {
        Ok(ViewRuleInsertPlan {
            relation: self.take(relation),
            supplied_columns: self
                .field("supplied_columns", |copying| Ok(copying.take(assigned)))?,
            input_columns: self.field("input_columns", |_| Ok(Vec::new()))?,
        })
    }
}
