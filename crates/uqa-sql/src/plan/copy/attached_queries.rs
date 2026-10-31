//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enclosing sources and set operations transfer admitted child roots without copying discarded bodies.

use super::{Copying, Result};
use crate::plan::{OwnedQueryPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

impl SourcePlan {
    /// Copy a subquery source's declaration while transferring the selected executable body and its root allocation.
    pub fn copy_with_query(
        &self,
        query: OwnedQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        control.check()?;
        let Self::Subquery {
            captured_output,
            lateral,
            alias,
            column_aliases,
            ..
        } = self
        else {
            return Err(SQLError::Internal(
                "query attachment requires a subquery source".into(),
            ));
        };
        let query = query.into_produced_box(control)?;
        let Some(mut copying) = Copying::new(*control) else {
            return Ok(control.finish(
                Self::Subquery {
                    captured_output: *captured_output,
                    lateral: *lateral,
                    body: query.into_uncontrolled().expect("ordinary source body"),
                    alias: alias.clone(),
                    column_aliases: column_aliases.clone(),
                },
                None,
            )?);
        };
        let source = Self::Subquery {
            captured_output: *captured_output,
            lateral: *lateral,
            body: copying.take(query),
            alias: copying.optional_text(alias.as_deref())?,
            column_aliases: copying.texts(column_aliases)?,
        };
        copying.finish(source)
    }
}

impl QueryPlan {
    /// Retain a set operation's declarations and clauses while transferring both independently optimized query inputs.
    pub fn copy_with_set_inputs(
        &self,
        left: OwnedQueryPlan,
        right: OwnedQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedQueryPlan> {
        control.check()?;
        let RelationalPlan::SetOp {
            kind,
            all,
            order_by,
            limit,
            with_ties,
            offset,
            subqueries,
            ..
        } = &self.root
        else {
            return Err(SQLError::Internal(
                "set input attachment requires a set operation".into(),
            ));
        };
        let left = left.into_produced_box(control)?;
        let right = right.into_produced_box(control)?;
        let Some(mut copying) = Copying::new(*control) else {
            let query = Self {
                analyzed_output: self.analyzed_output.clone(),
                analyzed_source: self.analyzed_source.clone(),
                analyzed_grouping: self.analyzed_grouping.clone(),
                relations_bound: self.relations_bound,
                relation_privilege_subject: self.relation_privilege_subject.clone(),
                ctes: self.ctes.clone(),
                root: RelationalPlan::SetOp {
                    kind: *kind,
                    all: *all,
                    left: left.into_uncontrolled().expect("ordinary left input"),
                    right: right.into_uncontrolled().expect("ordinary right input"),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    with_ties: *with_ties,
                    offset: offset.clone(),
                    subqueries: subqueries.clone(),
                },
            };
            return OwnedQueryPlan::from_produced(control.finish(query, None)?, control);
        };
        let query = copying.query_fields_using(self, |copying| {
            copying.set_operation_using(
                &self.root,
                |copying| Ok(copying.take(left)),
                |copying| Ok(copying.take(right)),
            )
        })?;
        OwnedQueryPlan::from_produced(copying.finish(query)?, control)
    }
}
