//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped and recursive WITH analysis without evaluating its statements.

use super::super::{
    cte_controls::{
        extend_cte_generated_schema_with_control, extend_recursive_cte_binding_schema_with_control,
    },
    projection::rename_schema_with_control,
};
use super::{error, Preparation, RowSchema, SQLError};
use crate::plan::{CtePlan, CtePlanBody, RelationalPlan};

impl Preparation<'_> {
    pub(super) fn ctes(
        &mut self,
        ctes: &[CtePlan],
        outer: Option<&RowSchema>,
    ) -> Result<super::super::ctes::CteScopeStack, SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let mut previous = super::super::ctes::CteScopeStack::new(ctes.len(), &control)?;
        let result = (|| {
            for (index, cte) in crate::semantics::order_cte_plans_with_control(ctes, control)?
                .iter()
                .copied()
                .enumerate()
            {
                let recursive =
                    crate::semantics::cte_references_own_name_with_control(cte, control)?;
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
                    let schema = rename_schema_with_control(
                        &seed.schema(&control)?,
                        &cte.columns,
                        None,
                        &control,
                    )?;
                    let schema = extend_recursive_cte_binding_schema_with_control(
                        self.routines,
                        cte,
                        schema,
                        &self.parameters.values()?,
                        &control,
                    )?;
                    previous.push(self.scope.install_cte_schema(cte, Some(schema))?);
                    let step = self.query_output(right, outer, true)?;
                    self.set_output(seed, step, *kind, *all)?
                        .schema(&self.scope.control())?
                } else {
                    let schema = match &cte.body {
                        CtePlanBody::Query(query) => self.query(query, outer),
                        CtePlanBody::Command(command) => {
                            self.command(command).map(Option::unwrap_or_default)
                        }
                    }
                    .map_err(|error| {
                        super::super::ctes::future_cte_reference_error(error, &ctes[index..])
                    })?;
                    previous.push(self.scope.install_cte_schema(cte, None)?);
                    schema
                };
                let schema = rename_schema_with_control(&schema, &cte.columns, None, &control)?;
                if let Some(cycle) = &cte.cycle {
                    let mut values = [
                        self.expression(&cycle.mark_value, &schema, &[])?,
                        self.expression(&cycle.mark_default, &schema, &[])?,
                    ];
                    self.common(
                        crate::type_resolution::CommonTypeContext::Cycle,
                        &mut values,
                    )?;
                }
                let schema = extend_cte_generated_schema_with_control(
                    self.routines,
                    cte,
                    schema,
                    &self.parameters.values()?,
                    &control,
                )?;
                self.scope.set_cte_schema(&cte.name, schema)?;
            }
            Ok::<_, SQLError>(())
        })();
        match result {
            Ok(()) => Ok(previous),
            Err(error) => {
                self.scope.restore_cte_schemas(previous);
                Err(error)
            }
        }
    }
}
