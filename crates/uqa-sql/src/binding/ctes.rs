//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped CTE result schemas for query and command plans.

use super::{
    cte_references_own_name, extend_cte_generated_schema, extend_recursive_cte_binding_schema,
    rename_schema, RoutineResolution, RowSchema, SQLError, SQLParam, SchemaScope,
};

impl SchemaScope {
    pub(super) fn set_cte_returning(&mut self, cte: &crate::plan::CtePlan) -> bool {
        let previous = self.non_returning_ctes.remove(&cte.name);
        if !cte.body.returns_rows() {
            self.non_returning_ctes.insert(cte.name.clone());
        }
        previous
    }

    pub(super) fn restore_cte_returning(&mut self, name: &str, previous: bool) {
        if previous {
            self.non_returning_ctes.insert(name.to_string());
        } else {
            self.non_returning_ctes.remove(name);
        }
    }

    pub(super) fn bind_cte_schemas(
        &mut self,
        routines: &dyn RoutineResolution,
        ctes: &[crate::plan::CtePlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<Vec<(String, bool, Option<RowSchema>)>, SQLError> {
        let mut previous = Vec::with_capacity(ctes.len());
        for (index, cte) in crate::semantics::ordered_cte_plans(ctes)?
            .into_iter()
            .enumerate()
        {
            let self_recursive = cte_references_own_name(cte);
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
            let provisional = rename_schema(&provisional, &cte.columns, None);
            let provisional = if self_recursive {
                extend_recursive_cte_binding_schema(routines, cte, provisional, params)?
            } else {
                extend_cte_generated_schema(routines, cte, provisional, params)?
            };
            previous.push((
                cte.name.clone(),
                self.set_cte_returning(cte),
                self.ctes.insert(cte.name.clone(), provisional),
            ));

            if self_recursive {
                let complete = self.bind_cte_body(routines, &cte.body, params, outer)?;
                let complete = rename_schema(&complete, &cte.columns, None);
                let complete = extend_cte_generated_schema(routines, cte, complete, params)?;
                self.ctes.insert(cte.name.clone(), complete);
            }
        }

        Ok(previous)
    }

    pub(super) fn restore_cte_schemas(&mut self, previous: Vec<(String, bool, Option<RowSchema>)>) {
        for (name, no_returning, schema) in previous.into_iter().rev() {
            self.restore_cte_returning(&name, no_returning);
            match schema {
                Some(schema) => {
                    self.ctes.insert(name, schema);
                }
                None => {
                    self.ctes.remove(&name);
                }
            }
        }
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
