//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful function-scan descriptions retained by one correlated source.

use super::{
    deferred_function_rows, initialize_function_source, qualify_source_operator_with_columns,
    validate_table_function_alias_count, CteScope, FunctionSource, InitializedFunctionSource,
    PhysicalOperator, PreparedFunctionCall, RowsFromOperator, SQLError, SQLParam, SourceContext,
    SourcePlan, TableFunctionPlan,
};
use crate::query::set_projection::SetFunctionRuntime;
use std::sync::{Arc, OnceLock};
use uqa_sql::routines::invocation::descriptions::RoutineDescriptionScope;

struct PreparedMember {
    columns: Vec<String>,
    schema: crate::RowSchema,
    call: Arc<PreparedFunctionCall>,
}

struct GroupDescription {
    scope: RoutineDescriptionScope,
    input_schema: crate::RowSchema,
    members: Vec<PreparedMember>,
    rows_schema: crate::RowSchema,
    columns: Vec<String>,
}

#[derive(Default)]
pub(in crate::query::sources) struct PreparedFunctionGroup {
    description: OnceLock<GroupDescription>,
}

impl PreparedFunctionGroup {
    pub(in crate::query::sources) fn operator<'a, S: Clone + Send + Sync + 'static>(
        &self,
        context: &SourceContext<'a, S>,
        source: &SourcePlan,
        params: &'a [SQLParam],
        ctes: &CteScope<S>,
        runtime: &Arc<dyn SetFunctionRuntime + 'a>,
        outer: &crate::OwnedPhysicalRow,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        self.build(
            source,
            params,
            ctes.lock_identities.emit,
            runtime,
            outer,
            |source| {
                initialize_function_source(context, source, params, runtime, Some(outer.clone()))
            },
        )
    }

    fn build<'a>(
        &self,
        source: &SourcePlan,
        params: &'a [SQLParam],
        emit_lock_origins: bool,
        runtime: &Arc<dyn SetFunctionRuntime + 'a>,
        outer: &crate::OwnedPhysicalRow,
        initialize: impl FnMut(&FunctionSource<'_>) -> Result<InitializedFunctionSource<'a>, SQLError>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        let SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } = source
        else {
            unreachable!("prepared group requires a function group")
        };
        let first = functions
            .first()
            .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
        let qualifier = alias.as_deref().unwrap_or(&first.output_name);
        let scope = runtime.routine_description_scope();
        let retained = self.description.get().filter(|saved| {
            scope
                .as_ref()
                .is_some_and(|scope| saved.scope.matches(scope))
                && saved.input_schema == outer.schema
        });
        let InitializedMembers { operators, members } = initialize_members(
            functions,
            retained,
            params,
            emit_lock_origins,
            runtime,
            outer,
            initialize,
        )?;
        let mut operator = match retained {
            Some(saved) => {
                RowsFromOperator::with_schema(operators, *ordinality, saved.rows_schema.clone())
            }
            None => RowsFromOperator::new(operators, *ordinality),
        };
        let columns = retained.map_or_else(
            || {
                operator
                    .row_schema()
                    .iter()
                    .enumerate()
                    .map(|(position, column)| {
                        operator
                            .row_schema()
                            .public_name(position)
                            .unwrap_or(column)
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            },
            |saved| saved.columns.clone(),
        );
        if let Err(error) =
            validate_table_function_alias_count(qualifier, columns.len(), column_aliases.len())
        {
            return Err(match operator.close() {
                Ok(()) => error,
                Err(cleanup) => error.with_cleanup(
                    crate::physical::physical_exec_error(cleanup),
                    "close correlated ROWS FROM after alias validation failure",
                ),
            });
        }
        if retained.is_none()
            && members.len() == functions.len()
            && functions
                .iter()
                .all(|function| function.binding.is_some() && function.column_types.is_empty())
        {
            if let Some(scope) = scope.filter(|scope| {
                runtime
                    .routine_description_scope()
                    .is_some_and(|current| scope.matches(&current))
            }) {
                let _ = self.description.set(GroupDescription {
                    scope,
                    input_schema: outer.schema.clone(),
                    members,
                    rows_schema: operator.row_schema().clone(),
                    columns: columns.clone(),
                });
            }
        }
        Ok(qualify_source_operator_with_columns(
            Box::new(operator),
            &columns,
            qualifier,
            None,
            column_aliases,
            emit_lock_origins,
        ))
    }
}

struct InitializedMembers<'a> {
    operators: Vec<Box<dyn PhysicalOperator + 'a>>,
    members: Vec<PreparedMember>,
}

fn initialize_members<'a>(
    functions: &[TableFunctionPlan],
    retained: Option<&GroupDescription>,
    params: &'a [SQLParam],
    emit_lock_origins: bool,
    runtime: &Arc<dyn SetFunctionRuntime + 'a>,
    outer: &crate::OwnedPhysicalRow,
    mut initialize: impl FnMut(&FunctionSource<'_>) -> Result<InitializedFunctionSource<'a>, SQLError>,
) -> Result<InitializedMembers<'a>, SQLError> {
    let mut operators: Vec<Box<dyn PhysicalOperator + 'a>> = Vec::with_capacity(functions.len());
    let mut members = Vec::with_capacity(functions.len());
    for (index, function) in functions.iter().enumerate() {
        let source = FunctionSource::group_member(function);
        let initialized = match retained {
            Some(saved) => {
                let member = &saved.members[index];
                InitializedFunctionSource {
                    columns: member.columns.clone(),
                    schema: member.schema.clone(),
                    rows: deferred_function_rows(
                        Arc::clone(&member.call),
                        Arc::clone(runtime),
                        params,
                        Some(outer.clone()),
                        member.columns.len(),
                    ),
                    prepared: None,
                }
            }
            None => match initialize(&source) {
                Ok(initialized) => initialized,
                Err(mut error) => {
                    for operator in &mut operators {
                        if let Err(cleanup) = operator.close() {
                            error = error.with_cleanup(crate::physical::physical_exec_error(cleanup), "close earlier correlated ROWS FROM member after initialization failure");
                        }
                    }
                    return Err(error);
                }
            },
        };
        if let Some(call) = &initialized.prepared {
            members.push(PreparedMember {
                columns: initialized.columns.clone(),
                schema: initialized.schema.clone(),
                call: Arc::clone(call),
            });
        }
        operators.push(initialized.operator(&source, None, emit_lock_origins));
    }
    Ok(InitializedMembers { operators, members })
}

#[cfg(test)]
#[path = "tests/group.rs"]
mod tests;
