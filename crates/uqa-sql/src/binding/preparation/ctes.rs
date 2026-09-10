//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped and recursive WITH analysis without evaluating its statements.

use super::super::{
    cte_references_own_name, extend_cte_generated_schema, extend_recursive_cte_binding_schema,
    rename_schema,
};
use super::{error, Preparation, RowSchema, SQLError};
use crate::plan::{CtePlan, CtePlanBody, RelationalPlan};

impl Preparation<'_> {
    pub(super) fn ctes(
        &mut self,
        ctes: &[CtePlan],
        outer: Option<&RowSchema>,
    ) -> Result<Vec<(String, bool, Option<RowSchema>)>, SQLError> {
        let mut previous = Vec::new();
        for cte in crate::semantics::ordered_cte_plans(ctes)? {
            let recursive = cte_references_own_name(cte);
            let schema = if recursive {
                let query = cte.body.query().ok_or_else(|| {
                    error(
                        "42P19",
                        format!(
                            "recursive query \"{}\" must not contain data-modifying statements",
                            cte.name
                        ),
                    )
                })?;
                let RelationalPlan::SetOp {
                    left,
                    right,
                    kind,
                    all,
                    ..
                } = &query.root
                else {
                    return Err(error("42P19", format!("recursive query \"{}\" does not have the form non-recursive-term UNION [ALL] recursive-term", cte.name)));
                };
                let seed = self.query_output(left, outer, false)?;
                let schema = rename_schema(&seed.schema(), &cte.columns, None);
                let schema = extend_recursive_cte_binding_schema(
                    self.routines,
                    cte,
                    schema,
                    &self.parameters.values(),
                )?;
                previous.push((
                    cte.name.clone(),
                    self.scope.set_cte_returning(cte),
                    self.scope.ctes.insert(cte.name.clone(), schema),
                ));
                let step = self.query_output(right, outer, true)?;
                self.set_output(seed, step, *kind, *all)?.schema()
            } else {
                let schema = match &cte.body {
                    CtePlanBody::Query(query) => self.query(query, outer)?,
                    CtePlanBody::Command(command) => self.command(command)?.unwrap_or_default(),
                };
                previous.push((
                    cte.name.clone(),
                    self.scope.set_cte_returning(cte),
                    self.scope.ctes.remove(&cte.name),
                ));
                schema
            };
            let schema = rename_schema(&schema, &cte.columns, None);
            if let Some(cycle) = &cte.cycle {
                let mut values = [
                    self.expression(&cycle.mark_value, &schema, &[])?,
                    self.expression(&cycle.mark_default, &schema, &[])?,
                ];
                self.common(&mut values)?;
            }
            let schema =
                extend_cte_generated_schema(self.routines, cte, schema, &self.parameters.values())?;
            self.scope.ctes.insert(cte.name.clone(), schema);
        }
        Ok(previous)
    }
}
