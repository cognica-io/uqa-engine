//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected query inputs for retained physical construction keep their constructor leases.

use super::{Copying, Result};
use crate::plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan};
use uqa_core::memory::{OwnedSet, Produced, ProductionControl};

impl QueryPlan {
    /// Copy the primary query after its WITH definitions have been installed in the lexical scope. Excluded declarations are never allocated; all primary metadata and nested query scopes retain their meaning.
    pub fn copy_primary_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        control.check()?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(
                Self {
                    analyzed_output: self.analyzed_output.clone(),
                    analyzed_source: self.analyzed_source.clone(),
                    analyzed_grouping: self.analyzed_grouping.clone(),
                    relations_bound: self.relations_bound,
                    relation_privilege_subject: self.relation_privilege_subject.clone(),
                    ctes: Vec::new(),
                    root: self.root.clone(),
                },
                None,
            )?);
        };
        let query =
            copying.query_fields_with_ctes(self, &[], |copying| copying.relational(&self.root))?;
        copying.finish(query)
    }

    /// Produce the ordering/slicing block of a set operation without copying either operand. Scalar-query slots remain in the same order as their original expressions.
    pub fn set_ordering_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<QueryBlockPlan>> {
        control.check()?;
        let RelationalPlan::SetOp {
            order_by,
            limit,
            offset,
            with_ties,
            subqueries,
            ..
        } = &self.root
        else {
            return Err(crate::SQLError::Internal(
                "set ordering requires a set-operation root".into(),
            ));
        };
        let mut copying = Copying::new(*control);
        let order_by = match copying.as_mut() {
            Some(copying) => copying.map(order_by.iter(), Copying::order)?,
            None => order_by.clone(),
        };
        let limit = match copying.as_mut() {
            Some(copying) => copying.optional_scalar(limit.as_deref())?,
            None => limit.as_deref().cloned(),
        };
        let offset = match copying.as_mut() {
            Some(copying) => copying.optional_scalar(offset.as_deref())?,
            None => offset.as_deref().cloned(),
        };
        let subqueries = match copying.as_mut() {
            Some(copying) => copying.map(subqueries.iter(), Copying::query)?,
            None => subqueries.clone(),
        };
        let block = QueryBlockPlan {
            deferred_outer_rewrites: Vec::new(),
            privilege_columns: OwnedSet::new(),
            projections: Vec::new(),
            from: None,
            r#where: None,
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by,
            limit,
            with_ties: *with_ties,
            offset,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries,
            access: AccessPathPlan::Row,
            locking: Vec::new(),
            windows: Vec::new(),
        };
        match copying {
            Some(copying) => copying.finish(block),
            None => Ok(control.finish(block, None)?),
        }
    }
}
