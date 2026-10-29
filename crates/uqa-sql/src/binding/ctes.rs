//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped CTE result schemas for query and command plans.

use super::{RoutineResolution, RowSchema, SQLError, SQLParam, SchemaScope};

mod ownership;
pub(super) use ownership::CteScopeStack;

impl SchemaScope {
    pub(super) fn bind_cte_schemas(
        &mut self,
        routines: &dyn RoutineResolution,
        ctes: &[crate::plan::CtePlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<CteScopeStack, SQLError> {
        let owner = self.control().capture();
        let control = owner.control();
        let mut previous = CteScopeStack::new(ctes.len(), &control)?;
        let result = (|| {
            for (index, cte) in crate::semantics::order_cte_plans_with_control(ctes, control)?
                .iter()
                .copied()
                .enumerate()
            {
                let self_recursive =
                    crate::semantics::cte_references_own_name_with_control(cte, control)?;
                // Execution describes an already analyzed definition in its original namespace. Rebinding it against a later same-name CTE would change its output type; fresh analysis still validates every input.
                let retained = (!self.validate_references)
                    .then(|| cte.body.query()?.analyzed_output.as_ref())
                    .flatten()
                    .filter(|schema| {
                        !schema.columns_are_open(None)
                            && schema.column_types().iter().all(Option::is_some)
                    });
                let provisional = if let Some(schema) = retained {
                    schema.clone()
                } else if self_recursive {
                    self.bind_recursive_seed(
                        routines,
                        cte.body.query().ok_or_else(|| SQLError::Routine {
                            sqlstate: "42P19".into(),
                            message: format!(
                                "recursive query \"{}\" must not contain data-modifying statements",
                                cte.name
                            ),
                        })?,
                        params,
                        outer,
                    )?
                } else {
                    self.bind_cte_body(routines, &cte.body, params, outer)
                        .map_err(|error| future_cte_reference_error(error, &ctes[index..]))?
                };
                let provisional = super::projection::rename_schema_with_control(
                    &provisional,
                    &cte.columns,
                    None,
                    &control,
                )?;
                let provisional = if self_recursive {
                    super::cte_controls::extend_recursive_cte_binding_schema_with_control(
                        routines,
                        cte,
                        provisional,
                        params,
                        &control,
                    )?
                } else {
                    super::cte_controls::extend_cte_generated_schema_with_control(
                        routines,
                        cte,
                        provisional,
                        params,
                        &control,
                    )?
                };
                previous.push(self.install_cte_schema(cte, Some(provisional))?);

                if self_recursive {
                    let complete = self.bind_cte_body(routines, &cte.body, params, outer)?;
                    let complete = super::projection::rename_schema_with_control(
                        &complete,
                        &cte.columns,
                        None,
                        &control,
                    )?;
                    let complete = super::cte_controls::extend_cte_generated_schema_with_control(
                        routines, cte, complete, params, &control,
                    )?;
                    self.set_cte_schema(&cte.name, complete)?;
                }
            }

            Ok::<_, SQLError>(())
        })();
        match result {
            Ok(()) => Ok(previous),
            Err(error) => {
                self.restore_cte_schemas(previous);
                Err(error)
            }
        }
    }

    pub(super) fn restore_cte_schemas(&mut self, previous: CteScopeStack) {
        self.restore_owned_cte_schemas(previous);
    }
}

/// Keep the original error position and context while explaining an ordinary WITH name that is not visible yet.
pub(super) fn future_cte_reference_error(
    error: SQLError,
    future: &[crate::plan::CtePlan],
) -> SQLError {
    match error {
        SQLError::UnknownTable(ref name)
            if crate::semantics::cte_reference_name(name).is_some_and(|name| {
                future.iter().any(|cte| !cte.recursive && cte.name == name)
            }) =>
        {
            let name = crate::semantics::cte_reference_name(name).expect("CTE reference checked");
            SQLError::Diagnostic {
                sqlstate: "42P01".into(),
                message: format!("relation \"{name}\" does not exist"),
                detail: Some(format!("There is a WITH item named \"{name}\", but it cannot be referenced from this part of the query.")),
                hint: Some("Use WITH RECURSIVE, or re-order the WITH items to remove forward references.".into()),
            }
        }
        SQLError::Position { source, position } => SQLError::Position {
            source: Box::new(future_cte_reference_error(*source, future)),
            position,
        },
        SQLError::InternalPosition {
            source,
            position,
            query,
        } => SQLError::InternalPosition {
            source: Box::new(future_cte_reference_error(*source, future)),
            position,
            query,
        },
        SQLError::Context { source, context } => SQLError::Context {
            source: Box::new(future_cte_reference_error(*source, future)),
            context,
        },
        error => error,
    }
}
