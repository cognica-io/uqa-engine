//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered FROM analysis, including lateral scopes and join coercion.

use super::super::{
    overlay_outer_schema, rename_schema, table_function_member_source, JoinSchemaBinding,
    SourcePlan,
};
use super::{Preparation, QueryPlan, RowSchema, SQLError};

impl Preparation<'_> {
    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        subqueries: &[QueryPlan],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match source {
            SourcePlan::Table { .. } => {}
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                ..
            } => {
                if internal_relation.is_some() {
                    return self.scope.bind_source(
                        self.routines,
                        source,
                        subqueries,
                        &self.parameters.values(),
                        outer,
                    );
                }
                let output = self.values(rows, subqueries, outer)?;
                return Ok(rename_schema(
                    &output.schema(),
                    column_aliases,
                    alias.as_deref(),
                ));
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let schema = self.query(body, outer)?;
                return Ok(rename_schema(&schema, column_aliases, alias.as_deref()));
            }
            SourcePlan::Function {
                name,
                binding,
                args,
                ..
            } => {
                let input = outer.cloned().unwrap_or_default();
                self.call(name, binding.as_ref(), args, &input, subqueries)?;
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    self.source(&table_function_member_source(function), subqueries, outer)?;
                }
            }
            SourcePlan::Join {
                left,
                right,
                kind,
                on,
                using,
                natural,
                alias,
                column_aliases,
                lateral,
                ..
            } => {
                let left = self.source(left, subqueries, outer)?;
                let right_scope = (*lateral
                    || matches!(
                        right.as_ref(),
                        SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                    ))
                .then(|| overlay_outer_schema(&left, outer));
                let right = self.source(right, subqueries, right_scope.as_ref().or(outer))?;
                if let Some(on) = on {
                    let joined = RowSchema::join(&left, &right, std::iter::empty::<String>());
                    let input = overlay_outer_schema(&joined, outer);
                    self.require_boolean(on, &input, subqueries, "JOIN/ON")?;
                }
                return self.scope.bind_join_output_schema(JoinSchemaBinding {
                    routines: self.routines,
                    kind: *kind,
                    on: None,
                    using: using.as_ref(),
                    natural: *natural,
                    alias: alias.as_deref(),
                    column_aliases,
                    left: &left,
                    right: &right,
                    subqueries,
                    params: &self.parameters.values(),
                    outer,
                });
            }
        }
        self.scope.bind_source(
            self.routines,
            source,
            subqueries,
            &self.parameters.values(),
            outer,
        )
    }
}
